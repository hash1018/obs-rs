//! Reading and writing the filters on a mixer channel.
//!
//! The audio twin of [`FilterStore`](super::FilterStore), over its own tables
//! — see migration 22 for why they are not the same ones. The operations are
//! the same five, and so is what they promise.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::domain::{
    AudioFilter, AudioFilterId, AudioFilterKind, AudioFilterSettings, AudioSourceId,
    NoiseGateSettings,
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
                noise_gate_filter_settings.release_ms
             FROM audio_source_filters
             LEFT JOIN noise_gate_filter_settings
                 ON noise_gate_filter_settings.filter_id = audio_source_filters.id
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
        match AudioFilterSettings::default_for(kind) {
            AudioFilterSettings::NoiseSuppression => {}
            AudioFilterSettings::NoiseGate(settings) => {
                write_noise_gate(transaction, id, settings)?;
            }
        }
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

    /// Replaces a gate's settings, put in order on the way in — see
    /// [`NoiseGateSettings::sanitised`]. Writes a row for a filter of another
    /// kind too, harmlessly: nothing reads a gate's settings for a filter
    /// whose kind says it is not one.
    pub(crate) fn set_noise_gate(
        transaction: &Transaction<'_>,
        id: AudioFilterId,
        settings: NoiseGateSettings,
    ) -> PersistenceResult<()> {
        write_noise_gate(transaction, id, settings)
    }
}

fn write_noise_gate(
    transaction: &Transaction<'_>,
    id: AudioFilterId,
    settings: NoiseGateSettings,
) -> PersistenceResult<()> {
    let settings = settings.sanitised();
    transaction.execute(
        "INSERT OR REPLACE INTO noise_gate_filter_settings
             (filter_id, open_threshold_db, close_threshold_db, attack_ms, hold_ms, release_ms)
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
                AudioFilterStore::set_noise_gate(transaction, gate, tuned)?;
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
}
