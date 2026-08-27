use rusqlite::{Connection, Transaction, params};

use crate::domain::{
    ColorSourceSettings, Crop, DisplayCaptureSettings, SceneCanvas, SceneId, SceneItem,
    SceneItemId, Source, SourceId, SourceKind, SourceSettings, Transform,
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
                display_capture_settings.monitor_name
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
                            monitor_name: row.get(24)?,
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
        monitor_name: &str,
    ) -> PersistenceResult<SceneItemId> {
        let name = unique_source_name(transaction, "Display Capture")?;
        let source_id = create(transaction, &name, SourceKind::DisplayCapture)?;
        transaction.execute(
            "INSERT INTO display_capture_settings (source_id, monitor_name)
             VALUES (?1, ?2)",
            params![source_id.0, monitor_name],
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
