use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::scene::SceneId;
use crate::snapshots::{SceneSnapshot, ScenesSnapshot};

use super::PersistenceResult;

pub(crate) struct SceneStore;

impl SceneStore {
    pub(crate) fn snapshot(connection: &Connection) -> PersistenceResult<ScenesSnapshot> {
        let selected_scene_id = connection
            .query_row(
                "SELECT selected_scene_id FROM app_state WHERE id = 1",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )?
            .map(SceneId);
        let mut statement =
            connection.prepare("SELECT id, name FROM scenes ORDER BY position, id")?;
        let items = statement
            .query_map([], |row| {
                Ok(SceneSnapshot {
                    id: SceneId(row.get(0)?),
                    name: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ScenesSnapshot {
            items,
            selected_scene_id,
        })
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
        select(transaction, SceneId(transaction.last_insert_rowid()))?;
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
