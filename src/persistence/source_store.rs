use rusqlite::Connection;
#[cfg(test)]
use rusqlite::{Transaction, params};

use crate::domain::{
    Crop, SceneId, SceneItem, SceneItemId, Source, SourceId, SourceKind, Transform,
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
                sources.kind
             FROM scene_items
             JOIN sources ON sources.id = scene_items.source_id
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
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    #[cfg(test)]
    pub(crate) fn create(
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

    #[cfg(test)]
    pub(crate) fn add_to_scene(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
        source_id: SourceId,
    ) -> PersistenceResult<SceneItemId> {
        transaction.execute(
            "INSERT INTO scene_items (scene_id, source_id, z_index)
             VALUES (
                ?1,
                ?2,
                COALESCE(
                    (SELECT MAX(z_index) + 1 FROM scene_items WHERE scene_id = ?1),
                    0
                )
             )",
            params![scene_id.0, source_id.0],
        )?;
        Ok(SceneItemId(transaction.last_insert_rowid()))
    }
}
