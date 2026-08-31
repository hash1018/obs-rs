use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::domain::{Scene, SceneId};

use super::PersistenceResult;

pub(crate) struct SceneStore;

impl SceneStore {
    pub(crate) fn list(connection: &Connection) -> PersistenceResult<Vec<Scene>> {
        let mut statement =
            connection.prepare("SELECT id, name FROM scenes ORDER BY position, id")?;
        Ok(statement
            .query_map([], |row| {
                Ok(Scene {
                    id: SceneId(row.get(0)?),
                    name: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub(crate) fn selected_scene_id(connection: &Connection) -> PersistenceResult<Option<SceneId>> {
        selected_scene_id(connection)
    }

    pub(crate) fn add(transaction: &Transaction<'_>) -> PersistenceResult<SceneId> {
        let name = next_scene_name(transaction)?;
        let position = next_position(transaction)?;
        transaction.execute(
            "INSERT INTO scenes (name, position) VALUES (?1, ?2)",
            params![name, position],
        )?;
        let id = SceneId(transaction.last_insert_rowid());
        select(transaction, id)?;
        Ok(id)
    }

    pub(crate) fn delete(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
    ) -> PersistenceResult<()> {
        let count: i64 =
            transaction.query_row("SELECT COUNT(*) FROM scenes", [], |row| row.get(0))?;
        if count <= 1 {
            return Ok(());
        }

        let selected = selected_scene_id(transaction)?;
        transaction.execute("DELETE FROM scenes WHERE id = ?1", [scene_id.0])?;
        // The Scene's items go with it — `scene_items.scene_id` cascades —
        // and a Source no item stands for any more goes too, the same as when
        // its last item is removed one at a time. A Source placed in another
        // Scene as well still has an item there, so this leaves it alone.
        // Without it the row would stay for the life of the project: invisible,
        // never opened, and still holding on to its name.
        transaction.execute(
            "DELETE FROM sources
             WHERE NOT EXISTS (
                 SELECT 1 FROM scene_items WHERE source_id = sources.id
             )",
            [],
        )?;
        normalize_positions(transaction)?;
        if selected == Some(scene_id) {
            let next_id = transaction.query_row(
                "SELECT id FROM scenes ORDER BY position, id LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            select(transaction, SceneId(next_id))?;
        }
        Ok(())
    }

    /// Copies a Scene, and everything placed in it.
    ///
    /// The items are copied; the Sources they stand for are not. Two Scenes
    /// then place the same Display Capture, which is what a Source belonging
    /// to the project rather than to one Scene means — one capture, opened
    /// once, wherever it is placed. Each copy keeps its own placement, so
    /// moving it in the copy leaves the original where it was.
    ///
    /// What the engine then does with a Source placed twice is the engine's:
    /// it opens a capture per SceneItem, and only the Scene being shown runs.
    /// A Windows display capture is the one that is shared between items,
    /// because a second `DuplicateOutput` of one output fails.
    pub(crate) fn duplicate(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
    ) -> PersistenceResult<()> {
        let Some((name, position)) = transaction
            .query_row(
                "SELECT name, position FROM scenes WHERE id = ?1",
                [scene_id.0],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            return Ok(());
        };
        let copy_name = unique_name(transaction, &format!("{name} Copy"))?;
        transaction.execute(
            "UPDATE scenes SET position = position + 1 WHERE position > ?1",
            [position],
        )?;
        transaction.execute(
            "INSERT INTO scenes (name, position) VALUES (?1, ?2)",
            params![copy_name, position + 1],
        )?;
        let copy_id = SceneId(transaction.last_insert_rowid());
        transaction.execute(
            "INSERT INTO scene_items
                (scene_id, source_id, visible, locked,
                 position_x, position_y, scale_x, scale_y, rotation_degrees,
                 anchor_x, anchor_y,
                 crop_left, crop_top, crop_right, crop_bottom, z_index)
             SELECT ?1, source_id, visible, locked,
                 position_x, position_y, scale_x, scale_y, rotation_degrees,
                 anchor_x, anchor_y,
                 crop_left, crop_top, crop_right, crop_bottom, z_index
             FROM scene_items
             WHERE scene_id = ?2",
            params![copy_id.0, scene_id.0],
        )?;
        select(transaction, copy_id)?;
        Ok(())
    }

    pub(crate) fn move_up(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
    ) -> PersistenceResult<()> {
        move_by(transaction, scene_id, -1)
    }

    pub(crate) fn move_down(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
    ) -> PersistenceResult<()> {
        move_by(transaction, scene_id, 1)
    }

    pub(crate) fn rename(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
        name: &str,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE scenes SET name = ?1 WHERE id = ?2",
            params![name, scene_id.0],
        )?;
        Ok(())
    }

    pub(crate) fn select(
        transaction: &Transaction<'_>,
        scene_id: SceneId,
    ) -> PersistenceResult<()> {
        select(transaction, scene_id)
    }
}

fn move_by(
    transaction: &Transaction<'_>,
    scene_id: SceneId,
    direction: i64,
) -> PersistenceResult<()> {
    let Some(position) = transaction
        .query_row(
            "SELECT position FROM scenes WHERE id = ?1",
            [scene_id.0],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(());
    };
    let target_position = position + direction;
    let target_id = transaction
        .query_row(
            "SELECT id FROM scenes WHERE position = ?1",
            [target_position],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(target_id) = target_id else {
        return Ok(());
    };

    transaction.execute(
        "UPDATE scenes
         SET position = CASE id WHEN ?1 THEN ?2 WHEN ?3 THEN ?4 END
         WHERE id IN (?1, ?3)",
        params![scene_id.0, target_position, target_id, position],
    )?;
    Ok(())
}

fn unique_name(connection: &Connection, base: &str) -> PersistenceResult<String> {
    let mut suffix = 1;
    loop {
        let candidate = if suffix == 1 {
            base.to_owned()
        } else {
            format!("{base} {suffix}")
        };
        let exists = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM scenes WHERE name = ?1)",
            [&candidate],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Ok(candidate);
        }
        suffix += 1;
    }
}

fn next_scene_name(connection: &Connection) -> PersistenceResult<String> {
    let mut number = 1;
    loop {
        let candidate = format!("Scene {number}");
        let exists = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM scenes WHERE name = ?1)",
            [&candidate],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Ok(candidate);
        }
        number += 1;
    }
}

fn next_position(connection: &Connection) -> PersistenceResult<i64> {
    Ok(connection.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM scenes",
        [],
        |row| row.get(0),
    )?)
}

fn normalize_positions(transaction: &Transaction<'_>) -> PersistenceResult<()> {
    let ids = {
        let mut statement = transaction.prepare("SELECT id FROM scenes ORDER BY position, id")?;
        statement
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (position, id) in ids.into_iter().enumerate() {
        transaction.execute(
            "UPDATE scenes SET position = ?1 WHERE id = ?2",
            params![position as i64, id],
        )?;
    }
    Ok(())
}

fn selected_scene_id(connection: &Connection) -> PersistenceResult<Option<SceneId>> {
    Ok(connection
        .query_row(
            "SELECT selected_scene_id FROM app_state WHERE id = 1",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?
        .map(SceneId))
}

fn select(connection: &Connection, scene_id: SceneId) -> PersistenceResult<()> {
    connection.execute(
        "UPDATE app_state
         SET selected_scene_id = ?1
         WHERE id = 1 AND EXISTS (SELECT 1 FROM scenes WHERE id = ?1)",
        [scene_id.0],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Transform;
    use crate::persistence::{ProjectDatabase, SourceStore};

    fn selected(database: &ProjectDatabase) -> SceneId {
        selected_scene_id(database.connection())
            .unwrap()
            .expect("a new project opens with a Scene selected")
    }

    fn source_ids(database: &ProjectDatabase, scene_id: SceneId) -> Vec<i64> {
        SourceStore::list_for_scene(database.connection(), scene_id)
            .unwrap()
            .into_iter()
            .map(|(_, source)| source.id.0)
            .collect()
    }

    /// A copy of a Scene with none of what was in it is an empty Scene with a
    /// borrowed name. What the copy shares is the Sources themselves — the
    /// same capture, placed twice — while the placements are its own.
    #[test]
    fn duplicating_a_scene_places_the_same_sources_in_the_copy() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let original = selected(&database);
        let item_id = database
            .transaction(|transaction| {
                SourceStore::add_drawing(transaction, original)?;
                SourceStore::add_color(transaction, original)
            })
            .unwrap();
        database
            .transaction(|transaction| {
                SourceStore::set_transform(
                    transaction,
                    item_id,
                    Transform {
                        position: [200.0, 100.0],
                        ..Transform::default()
                    },
                )
            })
            .unwrap();

        database
            .transaction(|transaction| SceneStore::duplicate(transaction, original))
            .unwrap();
        let copy = selected(&database);

        assert_ne!(copy, original, "duplicating selects the copy");
        assert_eq!(
            source_ids(&database, copy),
            source_ids(&database, original),
            "both Scenes place the same Sources, in the same order"
        );
        // The placement is copied, not shared: the copy starts where the
        // original was and moves on its own from there.
        let copied = SourceStore::list_for_scene(database.connection(), copy).unwrap();
        let placed = copied
            .iter()
            .find(|(item, _)| item.transform.position == [200.0, 100.0])
            .expect("the copy keeps where its items were placed");
        assert_ne!(placed.0.id, item_id, "the copy has items of its own");
    }

    /// A Source outlives the item that placed it only while another item
    /// still stands for it — the rule `delete_scene_item` follows one item at
    /// a time, and deleting a whole Scene has to follow it too.
    #[test]
    fn deleting_a_scene_keeps_a_source_the_other_scene_still_places() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let original = selected(&database);
        database
            .transaction(|transaction| SourceStore::add_color(transaction, original))
            .unwrap();
        database
            .transaction(|transaction| SceneStore::duplicate(transaction, original))
            .unwrap();
        let copy = selected(&database);

        database
            .transaction(|transaction| SceneStore::delete(transaction, original))
            .unwrap();

        assert_eq!(
            source_ids(&database, copy).len(),
            1,
            "the surviving Scene still places the Source"
        );
    }

    /// And a Source no Scene places any more goes with the Scene that held
    /// it. Left behind it would be invisible, never opened, and still holding
    /// the name a new Source would want.
    #[test]
    fn deleting_a_scene_takes_the_sources_nothing_else_places() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let first = selected(&database);
        let second = database.transaction(SceneStore::add).unwrap();
        database
            .transaction(|transaction| SourceStore::add_color(transaction, second))
            .unwrap();

        database
            .transaction(|transaction| SceneStore::delete(transaction, second))
            .unwrap();

        let remaining: i64 = database
            .connection()
            .query_row("SELECT COUNT(*) FROM sources", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0, "nothing places that Source any more");
        assert!(source_ids(&database, first).is_empty());
    }
}
