use rusqlite::Connection;

use super::database::PersistenceResult;

const SCHEMA_VERSION: i64 = 1;

pub(super) fn run(connection: &mut Connection) -> PersistenceResult<()> {
    let current_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current_version >= SCHEMA_VERSION {
        return Ok(());
    }

    let transaction = connection.transaction()?;
    if current_version < 1 {
        transaction.execute_batch(
            "CREATE TABLE scenes (
                id       INTEGER PRIMARY KEY,
                name     TEXT NOT NULL UNIQUE,
                position INTEGER NOT NULL
            );

            CREATE INDEX scenes_position_idx ON scenes(position);

            CREATE TABLE app_state (
                id                INTEGER PRIMARY KEY CHECK (id = 1),
                selected_scene_id INTEGER REFERENCES scenes(id) ON DELETE SET NULL
            );

            INSERT INTO scenes (name, position) VALUES ('Scene 1', 0);
            INSERT INTO app_state (id, selected_scene_id)
            VALUES (1, last_insert_rowid());

            PRAGMA user_version = 1;",
        )?;
    }
    transaction.commit()?;
    Ok(())
}
