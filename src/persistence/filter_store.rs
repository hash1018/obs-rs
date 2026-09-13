//! Reading and writing the filters on a Source.
//!
//! Kept apart from [`SourceStore`](super::source_store::SourceStore) because
//! the relationship is a different shape. Every other kind of settings is one
//! row per Source, which is why they can all be `LEFT JOIN`ed into the one
//! query that lists a Scene; filters are many rows per Source, which that
//! query cannot carry without multiplying every SceneItem by them. So they
//! are loaded in a second pass and attached.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::domain::{
    ChromaKeyMethod, ChromaKeySettings, ColorCorrectionSettings, Filter, FilterId, FilterKind,
    FilterSettings, LumaKeySettings, SourceId,
};

use super::database::PersistenceResult;

/// Which way a filter is being moved through its chain.
///
/// Earlier is nearer the Source. A chain is applied in order, so "up" in the
/// list the user sees is "earlier" here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterNeighbour {
    Earlier,
    Later,
}

pub(crate) struct FilterStore;

impl FilterStore {
    /// Every filter on `source_ids`, in the order each Source applies them.
    ///
    /// Sources with no filters are absent from the map rather than present
    /// with an empty list — the caller is building `Vec`s that default to
    /// empty anyway, and a row that was never written should not have to be
    /// represented.
    pub(crate) fn for_sources(
        connection: &Connection,
        source_ids: &[SourceId],
    ) -> PersistenceResult<HashMap<SourceId, Vec<Filter>>> {
        if source_ids.is_empty() {
            return Ok(HashMap::new());
        }

        // Built rather than bound, because SQLite has no array parameter and
        // the alternative is a query per Source. The values are row ids this
        // crate read out of its own database, so there is nothing here a
        // caller could have put a quote in.
        let placeholders = source_ids
            .iter()
            .map(|id| id.0.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT
                source_filters.id,
                source_filters.source_id,
                source_filters.kind,
                source_filters.enabled,
                chroma_key_filter_settings.method,
                chroma_key_filter_settings.custom_red,
                chroma_key_filter_settings.custom_green,
                chroma_key_filter_settings.custom_blue,
                chroma_key_filter_settings.threshold,
                chroma_key_filter_settings.smoothing,
                color_correction_filter_settings.brightness,
                color_correction_filter_settings.contrast,
                color_correction_filter_settings.saturation,
                color_correction_filter_settings.hue_degrees,
                color_correction_filter_settings.gamma,
                color_correction_filter_settings.opacity,
                luma_key_filter_settings.min,
                luma_key_filter_settings.min_smoothing,
                luma_key_filter_settings.max,
                luma_key_filter_settings.max_smoothing
             FROM source_filters
             LEFT JOIN chroma_key_filter_settings
                 ON chroma_key_filter_settings.filter_id = source_filters.id
             LEFT JOIN color_correction_filter_settings
                 ON color_correction_filter_settings.filter_id = source_filters.id
             LEFT JOIN luma_key_filter_settings
                 ON luma_key_filter_settings.filter_id = source_filters.id
             WHERE source_filters.source_id IN ({placeholders})
             ORDER BY source_filters.source_id, source_filters.position"
        );

        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([], |row| {
            let source_id = SourceId(row.get(1)?);
            let kind: String = row.get(2)?;
            let filter = Filter {
                id: FilterId(row.get(0)?),
                enabled: row.get(3)?,
                settings: match FilterKind::from_storage_name(&kind) {
                    Some(FilterKind::ChromaKey) => {
                        FilterSettings::ChromaKey(chroma_key_from_row(row)?)
                    }
                    Some(FilterKind::ColorCorrection) => {
                        FilterSettings::ColorCorrection(color_correction_from_row(row)?)
                    }
                    Some(FilterKind::LumaKey) => FilterSettings::LumaKey(luma_key_from_row(row)?),
                    // A kind this build does not know, which is what an
                    // older binary opening a newer project sees. Dropping
                    // the row is the only thing it can do with one, and it
                    // is not an error: the project is still readable.
                    None => return Ok(None),
                },
            };
            Ok(Some((source_id, filter)))
        })?;

        let mut by_source: HashMap<SourceId, Vec<Filter>> = HashMap::new();
        for row in rows {
            if let Some((source_id, filter)) = row? {
                by_source.entry(source_id).or_default().push(filter);
            }
        }
        Ok(by_source)
    }

    /// Appends one filter of `kind` to the end of `source_id`'s chain, with
    /// the settings that kind starts on.
    ///
    /// The end rather than the front: a filter that was just added and does
    /// nothing yet should not change what the ones already there receive.
    pub(crate) fn add(
        transaction: &Transaction<'_>,
        source_id: SourceId,
        kind: FilterKind,
    ) -> PersistenceResult<FilterId> {
        let next_position: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM source_filters WHERE source_id = ?1",
            params![source_id.0],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO source_filters (source_id, position, kind, enabled)
             VALUES (?1, ?2, ?3, 1)",
            params![source_id.0, next_position, kind.storage_name()],
        )?;
        let filter_id = FilterId(transaction.last_insert_rowid());
        write_settings(transaction, filter_id, FilterSettings::defaults(kind))?;
        Ok(filter_id)
    }

    /// Removes one filter. Its settings row goes with it, by cascade.
    ///
    /// The gap it leaves in `position` is not closed. Nothing reads those
    /// numbers except `ORDER BY`, and a chain of 0, 2, 3 applies in exactly
    /// the order 0, 1, 2 would.
    pub(crate) fn remove(
        transaction: &Transaction<'_>,
        filter_id: FilterId,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "DELETE FROM source_filters WHERE id = ?1",
            params![filter_id.0],
        )?;
        Ok(())
    }

    /// Swaps one filter with the neighbour on the given side, within its own
    /// Source's chain. A filter already at that end is left alone.
    pub(crate) fn swap_with_neighbour(
        transaction: &Transaction<'_>,
        filter_id: FilterId,
        neighbour: FilterNeighbour,
    ) -> PersistenceResult<()> {
        let Some((source_id, position)) = transaction
            .query_row(
                "SELECT source_id, position FROM source_filters WHERE id = ?1",
                params![filter_id.0],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            return Ok(());
        };

        let query = match neighbour {
            FilterNeighbour::Earlier => {
                "SELECT id, position FROM source_filters
                 WHERE source_id = ?1 AND position < ?2
                 ORDER BY position DESC LIMIT 1"
            }
            FilterNeighbour::Later => {
                "SELECT id, position FROM source_filters
                 WHERE source_id = ?1 AND position > ?2
                 ORDER BY position ASC LIMIT 1"
            }
        };
        let Some((other_id, other_position)) = transaction
            .query_row(query, params![source_id, position], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .optional()?
        else {
            return Ok(());
        };

        transaction.execute(
            "UPDATE source_filters
             SET position = CASE id WHEN ?1 THEN ?2 WHEN ?3 THEN ?4 END
             WHERE id IN (?1, ?3)",
            params![filter_id.0, other_position, other_id, position],
        )?;
        Ok(())
    }

    /// Turns one filter on or off, leaving its settings alone.
    pub(crate) fn set_enabled(
        transaction: &Transaction<'_>,
        filter_id: FilterId,
        enabled: bool,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE source_filters SET enabled = ?2 WHERE id = ?1",
            params![filter_id.0, enabled],
        )?;
        Ok(())
    }

    /// Replaces one filter's settings.
    ///
    /// Writes nothing if the settings are for another kind than the filter
    /// is, or the filter is gone: a settings row in the wrong table would be
    /// one nothing reads. A caller sending that has a bug, but not one worth
    /// failing a transaction over.
    pub(crate) fn set_settings(
        transaction: &Transaction<'_>,
        filter_id: FilterId,
        settings: FilterSettings,
    ) -> PersistenceResult<()> {
        let stored: Option<String> = transaction
            .query_row(
                "SELECT kind FROM source_filters WHERE id = ?1",
                params![filter_id.0],
                |row| row.get(0),
            )
            .optional()?;
        if stored.as_deref().and_then(FilterKind::from_storage_name) != Some(settings.kind()) {
            return Ok(());
        }
        write_settings(transaction, filter_id, settings)
    }
}

/// The settings row for whichever kind these are.
fn write_settings(
    transaction: &Transaction<'_>,
    filter_id: FilterId,
    settings: FilterSettings,
) -> PersistenceResult<()> {
    match settings {
        FilterSettings::ChromaKey(settings) => write_chroma_key(transaction, filter_id, settings),
        FilterSettings::ColorCorrection(settings) => {
            transaction.execute(
                "INSERT OR REPLACE INTO color_correction_filter_settings
                     (filter_id, brightness, contrast, saturation, hue_degrees, gamma, opacity)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    filter_id.0,
                    settings.brightness,
                    settings.contrast,
                    settings.saturation,
                    settings.hue_degrees,
                    settings.gamma,
                    settings.opacity,
                ],
            )?;
            Ok(())
        }
        FilterSettings::LumaKey(settings) => {
            transaction.execute(
                "INSERT OR REPLACE INTO luma_key_filter_settings
                     (filter_id, min, min_smoothing, max, max_smoothing)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    filter_id.0,
                    settings.min,
                    settings.min_smoothing,
                    settings.max,
                    settings.max_smoothing,
                ],
            )?;
            Ok(())
        }
    }
}

/// `INSERT OR REPLACE` rather than an insert and an update: the settings row
/// is created with the filter and replaced whenever it changes, and both
/// callers want exactly the row that is described here.
fn write_chroma_key(
    transaction: &Transaction<'_>,
    filter_id: FilterId,
    settings: ChromaKeySettings,
) -> PersistenceResult<()> {
    let [red, green, blue] = settings.custom_rgb;
    transaction.execute(
        "INSERT OR REPLACE INTO chroma_key_filter_settings
             (filter_id, method, custom_red, custom_green, custom_blue, threshold, smoothing)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            filter_id.0,
            settings.method.storage_name(),
            red,
            green,
            blue,
            settings.threshold,
            settings.smoothing,
        ],
    )?;
    Ok(())
}

/// Reads the chroma-key columns of a joined row.
///
/// A `source_filters` row whose kind says chroma key but whose settings row
/// is missing cannot be produced by this crate — the two are written in one
/// transaction — so its columns coming back NULL is a database somebody else
/// edited. It reads as the defaults rather than failing the whole project
/// load, which is the difference between one filter looking wrong and no
/// Scene opening at all.
fn chroma_key_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChromaKeySettings> {
    let defaults = ChromaKeySettings::default();
    let method: Option<String> = row.get(4)?;
    let method = method
        .as_deref()
        .and_then(ChromaKeyMethod::from_storage_name)
        .unwrap_or(defaults.method);
    Ok(ChromaKeySettings {
        method,
        custom_rgb: [
            row.get::<_, Option<u8>>(5)?
                .unwrap_or(defaults.custom_rgb[0]),
            row.get::<_, Option<u8>>(6)?
                .unwrap_or(defaults.custom_rgb[1]),
            row.get::<_, Option<u8>>(7)?
                .unwrap_or(defaults.custom_rgb[2]),
        ],
        threshold: row.get::<_, Option<f32>>(8)?.unwrap_or(defaults.threshold),
        smoothing: row.get::<_, Option<f32>>(9)?.unwrap_or(defaults.smoothing),
    })
}

/// The colour-correction columns of a joined row, defaults where they are
/// missing — see [`chroma_key_from_row`] for why a missing row is not an
/// error.
fn color_correction_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ColorCorrectionSettings> {
    let defaults = ColorCorrectionSettings::default();
    let value = |index: usize, default: f32| -> rusqlite::Result<f32> {
        Ok(row.get::<_, Option<f32>>(index)?.unwrap_or(default))
    };
    Ok(ColorCorrectionSettings {
        brightness: value(10, defaults.brightness)?,
        contrast: value(11, defaults.contrast)?,
        saturation: value(12, defaults.saturation)?,
        hue_degrees: value(13, defaults.hue_degrees)?,
        gamma: value(14, defaults.gamma)?,
        opacity: value(15, defaults.opacity)?,
    })
}

/// The luma-key columns of a joined row, defaults where they are missing.
fn luma_key_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LumaKeySettings> {
    let defaults = LumaKeySettings::default();
    let value = |index: usize, default: f32| -> rusqlite::Result<f32> {
        Ok(row.get::<_, Option<f32>>(index)?.unwrap_or(default))
    };
    Ok(LumaKeySettings {
        min: value(16, defaults.min)?,
        min_smoothing: value(17, defaults.min_smoothing)?,
        max: value(18, defaults.max)?,
        max_smoothing: value(19, defaults.max_smoothing)?,
    })
}
