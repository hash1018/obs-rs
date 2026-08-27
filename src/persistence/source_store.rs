use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::domain::{
    ColorSourceSettings, Crop, DisplayCaptureSettings, DisplayCaptureTarget, SceneCanvas, SceneId,
    SceneItem, SceneItemId, Source, SourceId, SourceKind, SourceSettings, Transform,
};

use super::PersistenceResult;

pub(crate) struct SourceStore;

impl SourceStore {
    pub(crate) fn list_for_scene(
        connection: &Connection,
        scene_id: SceneId,
    ) -> PersistenceResult<Vec<(SceneItem, Source)>> {
        let mut statement = connection.prepare(
            "SELECT
                scene_items.id,
                scene_items.source_id,
                scene_items.visible,
                scene_items.locked,
                scene_items.position_x,
                scene_items.position_y,
                scene_items.scale_x,
                scene_items.scale_y,
                scene_items.rotation_degrees,
                scene_items.anchor_x,
                scene_items.anchor_y,
                scene_items.crop_left,
                scene_items.crop_top,
                scene_items.crop_right,
                scene_items.crop_bottom,
                scene_items.z_index,
                sources.name,
                sources.kind,
                color_source_settings.width,
                color_source_settings.height,
                color_source_settings.red,
                color_source_settings.green,
                color_source_settings.blue,
                color_source_settings.alpha,
                display_capture_settings.target_kind,
                display_capture_settings.monitor_name,
                display_capture_settings.restore_token,
                display_capture_settings.width,
                display_capture_settings.height
             FROM scene_items
             JOIN sources ON sources.id = scene_items.source_id
             LEFT JOIN color_source_settings
                ON color_source_settings.source_id = sources.id
             LEFT JOIN display_capture_settings
                ON display_capture_settings.source_id = sources.id
             WHERE scene_items.scene_id = ?1
             ORDER BY scene_items.z_index DESC, scene_items.id DESC",
        )?;

        Ok(statement
            .query_map([scene_id.0], |row| {
                let kind_name: String = row.get(17)?;
                let kind = SourceKind::from_storage_name(&kind_name).ok_or_else(|| {
                    rusqlite::Error::InvalidColumnType(
                        17,
                        "kind".into(),
                        rusqlite::types::Type::Text,
                    )
                })?;
                let settings = match kind {
                    SourceKind::Color => SourceSettings::Color(ColorSourceSettings {
                        size: [row.get::<_, i64>(18)? as f32, row.get::<_, i64>(19)? as f32],
                        rgba: [
                            row.get::<_, i64>(20)? as u8,
                            row.get::<_, i64>(21)? as u8,
                            row.get::<_, i64>(22)? as u8,
                            row.get::<_, i64>(23)? as u8,
                        ],
                    }),
                    SourceKind::DisplayCapture => {
                        SourceSettings::DisplayCapture(DisplayCaptureSettings {
                            target: display_capture_target(
                                &row.get::<_, String>(24)?,
                                row.get(25)?,
                                row.get(26)?,
                            )
                            .ok_or_else(|| {
                                rusqlite::Error::InvalidColumnType(
                                    24,
                                    "target_kind".into(),
                                    rusqlite::types::Type::Text,
                                )
                            })?,
                            size_hint: match (row.get(27)?, row.get(28)?) {
                                (Some(width), Some(height)) => Some([width, height]),
                                _ => None,
                            },
                        })
                    }
                    _ => SourceSettings::None,
                };
                let source_id = SourceId(row.get(1)?);
                Ok((
                    SceneItem {
                        id: SceneItemId(row.get(0)?),
                        scene_id,
                        source_id,
                        visible: row.get(2)?,
                        locked: row.get(3)?,
                        transform: Transform {
                            position: [row.get(4)?, row.get(5)?],
                            scale: [row.get(6)?, row.get(7)?],
                            rotation_degrees: row.get(8)?,
                            anchor: [row.get(9)?, row.get(10)?],
                        },
                        crop: Crop {
                            left: row.get(11)?,
                            top: row.get(12)?,
                            right: row.get(13)?,
                            bottom: row.get(14)?,
                        },
                        z_index: row.get(15)?,
                    },
                    Source {
                        id: source_id,
                        name: row.get(16)?,
                        kind,
                        settings,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub(crate) fn add_color(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
    ) -> PersistenceResult<SceneItemId> {
        let name = unique_source_name(transaction, "Color Source")?;
        let source_id = create(transaction, &name, SourceKind::Color)?;
        let canvas = SceneCanvas::DEFAULT;
        let rgba = [53_u8, 91, 192, 255];
        transaction.execute(
            "INSERT INTO color_source_settings
                (source_id, width, height, red, green, blue, alpha)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                source_id.0,
                canvas.width as i64,
                canvas.height as i64,
                rgba[0],
                rgba[1],
                rgba[2],
                rgba[3]
            ],
        )?;
        add_to_scene(transaction, scene_id, source_id, canvas)
    }

    pub(crate) fn add_display_capture(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
        settings: &DisplayCaptureSettings,
    ) -> PersistenceResult<SceneItemId> {
        let name = unique_source_name(transaction, "Display Capture")?;
        let source_id = create(transaction, &name, SourceKind::DisplayCapture)?;
        let (kind, monitor_name, restore_token) = match &settings.target {
            DisplayCaptureTarget::MonitorName(monitor_name) => {
                ("monitor", Some(monitor_name.as_str()), None)
            }
            DisplayCaptureTarget::Portal { restore_token } => {
                ("portal", None, restore_token.as_deref())
            }
        };
        let [width, height] = settings
            .size_hint
            .map_or([None, None], |[width, height]| [Some(width), Some(height)]);
        transaction.execute(
            "INSERT INTO display_capture_settings
                (source_id, target_kind, monitor_name, restore_token, width, height)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                source_id.0,
                kind,
                monitor_name,
                restore_token,
                width,
                height
            ],
        )?;
        add_to_scene(transaction, scene_id, source_id, SceneCanvas::DEFAULT)
    }

    pub(crate) fn set_transform(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        transform: Transform,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE scene_items
             SET position_x = ?1,
                 position_y = ?2,
                 scale_x = ?3,
                 scale_y = ?4,
                 rotation_degrees = ?5,
                 anchor_x = ?6,
                 anchor_y = ?7
             WHERE id = ?8",
            params![
                transform.position[0],
                transform.position[1],
                transform.scale[0],
                transform.scale[1],
                transform.rotation_degrees,
                transform.anchor[0],
                transform.anchor[1],
                scene_item_id.0
            ],
        )?;
        Ok(())
    }

    pub(crate) fn set_visible(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        visible: bool,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE scene_items SET visible = ?1 WHERE id = ?2",
            params![visible, scene_item_id.0],
        )?;
        Ok(())
    }

    pub(crate) fn set_locked(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
        locked: bool,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE scene_items SET locked = ?1 WHERE id = ?2",
            params![locked, scene_item_id.0],
        )?;
        Ok(())
    }

    /// Moves an item in front of the one currently ahead of it.
    pub(crate) fn move_up(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
    ) -> PersistenceResult<()> {
        swap_with_neighbour(transaction, scene_item_id, Neighbour::Above)
    }

    /// Moves an item behind the one currently after it.
    pub(crate) fn move_down(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
    ) -> PersistenceResult<()> {
        swap_with_neighbour(transaction, scene_item_id, Neighbour::Below)
    }

    pub(crate) fn delete_scene_item(
        transaction: &Transaction<'_>,
        scene_item_id: SceneItemId,
    ) -> PersistenceResult<()> {
        let source_id = transaction.query_row(
            "SELECT source_id FROM scene_items WHERE id = ?1",
            [scene_item_id.0],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.execute("DELETE FROM scene_items WHERE id = ?1", [scene_item_id.0])?;
        transaction.execute(
            "DELETE FROM sources
             WHERE id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM scene_items WHERE source_id = ?1
               )",
            [source_id],
        )?;
        Ok(())
    }
}

/// Rebuilds the target a Display Capture row stores. The schema's own CHECK
/// constraint already rejects the combinations this cannot map, so `None` here
/// means a row written by something other than [`SourceStore`].
fn display_capture_target(
    kind: &str,
    monitor_name: Option<String>,
    restore_token: Option<String>,
) -> Option<DisplayCaptureTarget> {
    match kind {
        "monitor" => monitor_name.map(DisplayCaptureTarget::MonitorName),
        "portal" => Some(DisplayCaptureTarget::Portal { restore_token }),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Neighbour {
    /// Composited in front, which is a higher `z_index`.
    Above,
    Below,
}

/// Exchanges an item's compositing order with its adjacent neighbour.
///
/// The neighbour is found by ordering rather than by an expected `z_index`
/// value: deleting an item leaves a gap, so the two are not adjacent numbers
/// for long, and looking one up by `z_index + 1` would silently do nothing.
fn swap_with_neighbour(
    transaction: &Transaction<'_>,
    scene_item_id: SceneItemId,
    neighbour: Neighbour,
) -> PersistenceResult<()> {
    let Some((scene_id, z_index)) = transaction
        .query_row(
            "SELECT scene_id, z_index FROM scene_items WHERE id = ?1",
            [scene_item_id.0],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
    else {
        return Ok(());
    };

    let query = match neighbour {
        Neighbour::Above => {
            "SELECT id, z_index FROM scene_items
             WHERE scene_id = ?1 AND z_index > ?2
             ORDER BY z_index ASC LIMIT 1"
        }
        Neighbour::Below => {
            "SELECT id, z_index FROM scene_items
             WHERE scene_id = ?1 AND z_index < ?2
             ORDER BY z_index DESC LIMIT 1"
        }
    };
    let Some((other_id, other_z)) = transaction
        .query_row(query, params![scene_id, z_index], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .optional()?
    else {
        return Ok(());
    };

    transaction.execute(
        "UPDATE scene_items
         SET z_index = CASE id WHEN ?1 THEN ?2 WHEN ?3 THEN ?4 END
         WHERE id IN (?1, ?3)",
        params![scene_item_id.0, other_z, other_id, z_index],
    )?;
    Ok(())
}

fn create(
    transaction: &Transaction<'_>,
    name: &str,
    kind: SourceKind,
) -> PersistenceResult<SourceId> {
    transaction.execute(
        "INSERT INTO sources (name, kind) VALUES (?1, ?2)",
        params![name, kind.storage_name()],
    )?;
    Ok(SourceId(transaction.last_insert_rowid()))
}

fn add_to_scene(
    transaction: &Transaction<'_>,
    scene_id: SceneId,
    source_id: SourceId,
    canvas: SceneCanvas,
) -> PersistenceResult<SceneItemId> {
    transaction.execute(
        "INSERT INTO scene_items
                (scene_id, source_id, position_x, position_y, z_index)
             VALUES (
                ?1,
                ?2,
                ?3,
                ?4,
                COALESCE(
                    (SELECT MAX(z_index) + 1 FROM scene_items WHERE scene_id = ?1),
                    0
                )
             )",
        params![
            scene_id.0,
            source_id.0,
            canvas.width * 0.5,
            canvas.height * 0.5
        ],
    )?;
    Ok(SceneItemId(transaction.last_insert_rowid()))
}

fn unique_source_name(connection: &Connection, base: &str) -> PersistenceResult<String> {
    let mut suffix = 1;
    loop {
        let candidate = if suffix == 1 {
            base.to_owned()
        } else {
            format!("{base} {suffix}")
        };
        let exists = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sources WHERE name = ?1)",
            [&candidate],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Ok(candidate);
        }
        suffix += 1;
    }
}
