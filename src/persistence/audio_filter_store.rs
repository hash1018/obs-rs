//! Reading and writing the filters on a mixer channel.
//!
//! The audio twin of [`FilterStore`](super::FilterStore), over its own tables
//! — see migration 22 for why they are not the same ones. The operations are
//! the same five, and so is what they promise.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::domain::{
    AudioFilter, AudioFilterId, AudioFilterKind, AudioFilterSettings, AudioSourceId,
    CompressorSettings, LimiterSettings, NoiseGateSettings,
};

use super::database::PersistenceResult;
use super::filter_store::FilterNeighbour;

pub(crate) struct AudioFilterStore;

impl AudioFilterStore {
    /// Every channel's filters, in the order each applies them. A channel
    /// with none is absent rather than present with an empty list.
    pub(crate) fn all(
        connection: &Connection,
    ) -> PersistenceResult<HashMap<AudioSourceId, Vec<AudioFilter>>> {
        let mut statement = connection.prepare(
            "SELECT
                audio_source_filters.id,
                audio_source_filters.audio_source_id,
                audio_source_filters.kind,
                audio_source_filters.enabled,
                noise_gate_filter_settings.open_threshold_db,
                noise_gate_filter_settings.close_threshold_db,
                noise_gate_filter_settings.attack_ms,
                noise_gate_filter_settings.hold_ms,
                noise_gate_filter_settings.release_ms,
                compressor_filter_settings.threshold_db,
                compressor_filter_settings.ratio,
                compressor_filter_settings.attack_ms,
                compressor_filter_settings.release_ms,
                compressor_filter_settings.output_gain_db,
                limiter_filter_settings.threshold_db,
                limiter_filter_settings.release_ms
             FROM audio_source_filters
             LEFT JOIN noise_gate_filter_settings
                 ON noise_gate_filter_settings.filter_id = audio_source_filters.id
             LEFT JOIN compressor_filter_settings
                 ON compressor_filter_settings.filter_id = audio_source_filters.id
             LEFT JOIN limiter_filter_settings
                 ON limiter_filter_settings.filter_id = audio_source_filters.id
             ORDER BY audio_source_filters.audio_source_id, audio_source_filters.position",
        )?;
        let rows = statement.query_map([], |row| {
            let owner = AudioSourceId(row.get(1)?);
            let kind: String = row.get(2)?;
            let settings = match AudioFilterKind::from_storage_name(&kind) {
                Some(AudioFilterKind::NoiseSuppression) => AudioFilterSettings::NoiseSuppression,
                Some(AudioFilterKind::NoiseGate) => {
                    AudioFilterSettings::NoiseGate(noise_gate_from_row(row)?)
                }
                Some(AudioFilterKind::Compressor) => {
                    AudioFilterSettings::Compressor(compressor_from_row(row)?)
                }
                Some(AudioFilterKind::Limiter) => {
                    AudioFilterSettings::Limiter(limiter_from_row(row)?)
                }
                // A kind a newer build wrote. Dropped, as `FilterStore`
                // drops one: the project is still readable without it.
                None => return Ok(None),
            };
            Ok(Some((
                owner,
                AudioFilter {
                    id: AudioFilterId(row.get(0)?),
                    enabled: row.get(3)?,
                    settings,
                },
            )))
        })?;

        let mut by_owner: HashMap<AudioSourceId, Vec<AudioFilter>> = HashMap::new();
        for row in rows {
            if let Some((owner, filter)) = row? {
                by_owner.entry(owner).or_default().push(filter);
            }
        }
        Ok(by_owner)
    }

    /// Appends one filter of `kind` to the end of the channel's chain, with
    /// the settings that kind starts on — the end, so the filters already
    /// there hear what they heard before.
    pub(crate) fn add(
        transaction: &Transaction<'_>,
        owner: AudioSourceId,
        kind: AudioFilterKind,
    ) -> PersistenceResult<AudioFilterId> {
        let next_position: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM audio_source_filters
             WHERE audio_source_id = ?1",
            params![owner.0],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO audio_source_filters (audio_source_id, position, kind, enabled)
             VALUES (?1, ?2, ?3, 1)",
            params![owner.0, next_position, kind.storage_name()],
        )?;
        let id = AudioFilterId(transaction.last_insert_rowid());
        write_settings(transaction, id, AudioFilterSettings::default_for(kind))?;
        Ok(id)
    }

    /// Removes one filter, and its settings row with it by cascade.
    pub(crate) fn remove(
        transaction: &Transaction<'_>,
        id: AudioFilterId,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "DELETE FROM audio_source_filters WHERE id = ?1",
            params![id.0],
        )?;
        Ok(())
    }

    /// Swaps one filter with its neighbour on the given side, within its own
    /// channel. One already at that end is left alone.
    pub(crate) fn swap_with_neighbour(
        transaction: &Transaction<'_>,
        id: AudioFilterId,
        neighbour: FilterNeighbour,
    ) -> PersistenceResult<()> {
        let Some((owner, position)) = transaction
            .query_row(
                "SELECT audio_source_id, position FROM audio_source_filters WHERE id = ?1",
                params![id.0],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            return Ok(());
        };
        let query = match neighbour {
            FilterNeighbour::Earlier => {
                "SELECT id, position FROM audio_source_filters
                 WHERE audio_source_id = ?1 AND position < ?2
                 ORDER BY position DESC LIMIT 1"
            }
            FilterNeighbour::Later => {
                "SELECT id, position FROM audio_source_filters
                 WHERE audio_source_id = ?1 AND position > ?2
                 ORDER BY position ASC LIMIT 1"
            }
        };
        let Some((other_id, other_position)) = transaction
            .query_row(query, params![owner, position], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .optional()?
        else {
            return Ok(());
        };
        transaction.execute(
            "UPDATE audio_source_filters
             SET position = CASE id WHEN ?1 THEN ?2 WHEN ?3 THEN ?4 END
             WHERE id IN (?1, ?3)",
            params![id.0, other_position, other_id, position],
        )?;
        Ok(())
    }

    /// Turns one filter on or off, leaving its settings alone.
    pub(crate) fn set_enabled(
        transaction: &Transaction<'_>,
        id: AudioFilterId,
        enabled: bool,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE audio_source_filters SET enabled = ?2 WHERE id = ?1",
            params![id.0, enabled],
        )?;
        Ok(())
    }

    /// Replaces a filter's settings, brought within range on the way in —
    /// see [`AudioFilterSettings::sanitised`]. Settings of another kind than
    /// the filter's are written too, harmlessly: nothing reads a gate's
    /// settings for a filter whose kind says it is not one.
    pub(crate) fn set_settings(
        transaction: &Transaction<'_>,
        id: AudioFilterId,
        settings: AudioFilterSettings,
    ) -> PersistenceResult<()> {
        write_settings(transaction, id, settings)
    }
}

fn write_settings(
    transaction: &Transaction<'_>,
    id: AudioFilterId,
    settings: AudioFilterSettings,
) -> PersistenceResult<()> {
    match settings.sanitised() {
        AudioFilterSettings::NoiseSuppression => {}
        AudioFilterSettings::NoiseGate(settings) => {
            transaction.execute(
                "INSERT OR REPLACE INTO noise_gate_filter_settings
                     (filter_id, open_threshold_db, close_threshold_db, attack_ms, hold_ms,
                      release_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id.0,
                    settings.open_threshold_db,
                    settings.close_threshold_db,
                    settings.attack_ms,
                    settings.hold_ms,
                    settings.release_ms,
                ],
            )?;
        }
        AudioFilterSettings::Compressor(settings) => {
            transaction.execute(
                "INSERT OR REPLACE INTO compressor_filter_settings
                     (filter_id, threshold_db, ratio, attack_ms, release_ms, output_gain_db)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id.0,
                    settings.threshold_db,
                    settings.ratio,
                    settings.attack_ms,
                    settings.release_ms,
                    settings.output_gain_db,
                ],
            )?;
        }
        AudioFilterSettings::Limiter(settings) => {
            transaction.execute(
                "INSERT OR REPLACE INTO limiter_filter_settings
                     (filter_id, threshold_db, release_ms)
                 VALUES (?1, ?2, ?3)",
                params![id.0, settings.threshold_db, settings.release_ms],
            )?;
        }
    }
    Ok(())
}

/// Reads the gate's columns of a joined row, falling back to the defaults
/// for a settings row that is missing — a database somebody else edited,
/// which should cost one filter its tuning rather than the project its load.
fn noise_gate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoiseGateSettings> {
    let defaults = NoiseGateSettings::default();
    Ok(NoiseGateSettings {
        open_threshold_db: row
            .get::<_, Option<f32>>(4)?
            .unwrap_or(defaults.open_threshold_db),
        close_threshold_db: row
            .get::<_, Option<f32>>(5)?
            .unwrap_or(defaults.close_threshold_db),
        attack_ms: row.get::<_, Option<u32>>(6)?.unwrap_or(defaults.attack_ms),
        hold_ms: row.get::<_, Option<u32>>(7)?.unwrap_or(defaults.hold_ms),
        release_ms: row.get::<_, Option<u32>>(8)?.unwrap_or(defaults.release_ms),
    }
    .sanitised())
}

/// The compressor's columns, with the gate's fallback to defaults.
fn compressor_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CompressorSettings> {
    let defaults = CompressorSettings::default();
    Ok(CompressorSettings {
        threshold_db: row
            .get::<_, Option<f32>>(9)?
            .unwrap_or(defaults.threshold_db),
        ratio: row.get::<_, Option<f32>>(10)?.unwrap_or(defaults.ratio),
        attack_ms: row.get::<_, Option<u32>>(11)?.unwrap_or(defaults.attack_ms),
        release_ms: row
            .get::<_, Option<u32>>(12)?
            .unwrap_or(defaults.release_ms),
        output_gain_db: row
            .get::<_, Option<f32>>(13)?
            .unwrap_or(defaults.output_gain_db),
    }
    .sanitised())
}

/// The limiter's columns, with the gate's fallback to defaults.
fn limiter_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LimiterSettings> {
    let defaults = LimiterSettings::default();
    Ok(LimiterSettings {
        threshold_db: row
            .get::<_, Option<f32>>(14)?
            .unwrap_or(defaults.threshold_db),
        release_ms: row
            .get::<_, Option<u32>>(15)?
            .unwrap_or(defaults.release_ms),
    }
    .sanitised())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{AudioStore, ProjectDatabase};

    fn microphone(database: &ProjectDatabase) -> AudioSourceId {
        AudioStore::list(database.connection()).unwrap()[1].id
    }

    /// Added at the end, read back in order, reordered by a swap within the
    /// channel, and gone once removed — the chain a mixer channel keeps.
    #[test]
    fn a_channels_filters_are_kept_in_order_and_can_be_reordered() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let mic = microphone(&database);
        let (suppression, gate) = database
            .transaction(|transaction| {
                Ok((
                    AudioFilterStore::add(transaction, mic, AudioFilterKind::NoiseSuppression)?,
                    AudioFilterStore::add(transaction, mic, AudioFilterKind::NoiseGate)?,
                ))
            })
            .unwrap();

        let order = |database: &ProjectDatabase| -> Vec<AudioFilterId> {
            AudioFilterStore::all(database.connection()).unwrap()[&mic]
                .iter()
                .map(|filter| filter.id)
                .collect()
        };
        assert_eq!(order(&database), [suppression, gate]);
        assert_eq!(
            AudioFilterStore::all(database.connection()).unwrap()[&mic][1].settings,
            AudioFilterSettings::NoiseGate(NoiseGateSettings::default()),
            "a gate starts on the defaults"
        );

        database
            .transaction(|transaction| {
                AudioFilterStore::swap_with_neighbour(transaction, gate, FilterNeighbour::Earlier)
            })
            .unwrap();
        assert_eq!(order(&database), [gate, suppression]);

        database
            .transaction(|transaction| AudioFilterStore::remove(transaction, gate))
            .unwrap();
        assert_eq!(order(&database), [suppression]);
    }

    /// Turning a gate off keeps its tuning, and what is stored is put in
    /// order on the way in.
    #[test]
    fn a_gates_settings_are_kept_in_order_and_survive_being_turned_off() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let mic = microphone(&database);
        let gate = database
            .transaction(|transaction| {
                AudioFilterStore::add(transaction, mic, AudioFilterKind::NoiseGate)
            })
            .unwrap();
        let tuned = NoiseGateSettings {
            open_threshold_db: -40.0,
            close_threshold_db: -20.0,
            hold_ms: 350,
            ..NoiseGateSettings::default()
        };
        database
            .transaction(|transaction| {
                AudioFilterStore::set_settings(
                    transaction,
                    gate,
                    AudioFilterSettings::NoiseGate(tuned),
                )?;
                AudioFilterStore::set_enabled(transaction, gate, false)
            })
            .unwrap();

        let filter = &AudioFilterStore::all(database.connection()).unwrap()[&mic][0];
        assert!(!filter.enabled);
        assert_eq!(
            filter.settings,
            AudioFilterSettings::NoiseGate(NoiseGateSettings {
                close_threshold_db: -40.0,
                ..tuned
            })
        );
    }

    /// Each kind's settings come back from its own table, on its own
    /// filter — three settings tables joined onto one row each must not
    /// hand one filter another's columns.
    #[test]
    fn a_compressor_and_a_limiter_keep_their_own_settings_beside_a_gate() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let mic = microphone(&database);
        let compressor = CompressorSettings {
            threshold_db: -24.0,
            ratio: 4.0,
            attack_ms: 12,
            release_ms: 250,
            output_gain_db: 6.0,
        };
        let limiter = LimiterSettings {
            threshold_db: -2.0,
            release_ms: 90,
        };
        database
            .transaction(|transaction| {
                AudioFilterStore::add(transaction, mic, AudioFilterKind::NoiseGate)?;
                let first = AudioFilterStore::add(transaction, mic, AudioFilterKind::Compressor)?;
                let second = AudioFilterStore::add(transaction, mic, AudioFilterKind::Limiter)?;
                AudioFilterStore::set_settings(
                    transaction,
                    first,
                    AudioFilterSettings::Compressor(compressor),
                )?;
                AudioFilterStore::set_settings(
                    transaction,
                    second,
                    AudioFilterSettings::Limiter(limiter),
                )
            })
            .unwrap();

        let settings: Vec<AudioFilterSettings> = AudioFilterStore::all(database.connection())
            .unwrap()[&mic]
            .iter()
            .map(|filter| filter.settings)
            .collect();
        assert_eq!(
            settings,
            [
                AudioFilterSettings::NoiseGate(NoiseGateSettings::default()),
                AudioFilterSettings::Compressor(compressor),
                AudioFilterSettings::Limiter(limiter),
            ]
        );
    }
}
