use rusqlite::Connection;

use super::database::PersistenceResult;

const SCHEMA_VERSION: i64 = 2;

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
    if current_version < 2 {
        transaction.execute_batch(
            "CREATE TABLE sources (
                id            INTEGER PRIMARY KEY,
                name          TEXT NOT NULL UNIQUE,
                kind          TEXT NOT NULL,
                settings_json TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE scene_items (
                id               INTEGER PRIMARY KEY,
                scene_id         INTEGER NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
                source_id        INTEGER NOT NULL REFERENCES sources(id) ON DELETE RESTRICT,
                visible          INTEGER NOT NULL DEFAULT 1,
                locked           INTEGER NOT NULL DEFAULT 0,
                position_x       REAL NOT NULL DEFAULT 0,
                position_y       REAL NOT NULL DEFAULT 0,
                scale_x          REAL NOT NULL DEFAULT 1,
                scale_y          REAL NOT NULL DEFAULT 1,
                rotation_degrees REAL NOT NULL DEFAULT 0,
                anchor_x         REAL NOT NULL DEFAULT 0.5,
                anchor_y         REAL NOT NULL DEFAULT 0.5,
                crop_left        REAL NOT NULL DEFAULT 0,
                crop_top         REAL NOT NULL DEFAULT 0,
                crop_right       REAL NOT NULL DEFAULT 0,
                crop_bottom      REAL NOT NULL DEFAULT 0,
                z_index          INTEGER NOT NULL
            );

            CREATE INDEX scene_items_scene_z_idx
            ON scene_items(scene_id, z_index DESC);

            CREATE INDEX scene_items_source_idx
            ON scene_items(source_id);

            PRAGMA user_version = 2;",
        )?;
    }
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_one_database_is_upgraded_without_losing_scenes() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE scenes (
                    id       INTEGER PRIMARY KEY,
                    name     TEXT NOT NULL UNIQUE,
                    position INTEGER NOT NULL
                );
                INSERT INTO scenes (name, position) VALUES ('Existing Scene', 0);
                PRAGMA user_version = 1;",
            )
            .unwrap();

        run(&mut connection).unwrap();

        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let scene_name: String = connection
            .query_row("SELECT name FROM scenes", [], |row| row.get(0))
            .unwrap();
        let source_table_exists: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'sources'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(scene_name, "Existing Scene");
        assert!(source_table_exists);
    }
}
