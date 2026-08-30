use rusqlite::Connection;

use super::database::PersistenceResult;

const SCHEMA_VERSION: i64 = 9;

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
    if current_version < 3 {
        transaction.execute_batch(
            "CREATE TABLE color_source_settings (
                source_id INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                width     INTEGER NOT NULL,
                height    INTEGER NOT NULL,
                red       INTEGER NOT NULL CHECK (red BETWEEN 0 AND 255),
                green     INTEGER NOT NULL CHECK (green BETWEEN 0 AND 255),
                blue      INTEGER NOT NULL CHECK (blue BETWEEN 0 AND 255),
                alpha     INTEGER NOT NULL CHECK (alpha BETWEEN 0 AND 255)
            );

            PRAGMA user_version = 3;",
        )?;
    }
    if current_version < 4 {
        transaction.execute_batch(
            "CREATE TABLE display_capture_settings (
                source_id    INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                monitor_name TEXT NOT NULL
            );

            PRAGMA user_version = 4;",
        )?;
    }
    if current_version < 5 {
        // A Wayland selection has no display name to store. Version 4 stuffed
        // the portal's stream id into `monitor_name` as a placeholder, but a
        // stream id belongs to the session that produced it and means nothing
        // to a later one — the portal's restore token is the only value that
        // reproduces a selection. Splitting the two apart needs `monitor_name`
        // to become nullable, which SQLite only does by rebuilding the table.
        //
        // Placeholder rows carry no token and cannot be given one, so they
        // become portal targets that prompt again on first capture.
        transaction.execute_batch(
            "CREATE TABLE display_capture_settings_new (
                source_id     INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                target_kind   TEXT NOT NULL CHECK (target_kind IN ('monitor', 'portal')),
                monitor_name  TEXT,
                restore_token TEXT,
                CHECK (
                    (target_kind = 'monitor'
                        AND monitor_name IS NOT NULL
                        AND restore_token IS NULL)
                 OR (target_kind = 'portal' AND monitor_name IS NULL)
                )
            );

            INSERT INTO display_capture_settings_new
                (source_id, target_kind, monitor_name)
            SELECT
                source_id,
                CASE WHEN monitor_name LIKE 'portal: %'
                       OR monitor_name LIKE 'portal-node: %'
                     THEN 'portal' ELSE 'monitor' END,
                CASE WHEN monitor_name LIKE 'portal: %'
                       OR monitor_name LIKE 'portal-node: %'
                     THEN NULL ELSE monitor_name END
            FROM display_capture_settings;

            DROP TABLE display_capture_settings;

            ALTER TABLE display_capture_settings_new
            RENAME TO display_capture_settings;

            PRAGMA user_version = 5;",
        )?;
    }
    if current_version < 6 {
        // Every picker already knows the display's size and shows it to the
        // user; only the stored source did not. Without it a SceneItem stands
        // in at Canvas size, so an ultrawide display starts at the wrong aspect
        // ratio and visibly changes shape the moment capture opens.
        //
        // Nullable because a picker may report no size, and because rows
        // written before this migration have none. Both fall back to Canvas
        // size, which is what they already did.
        transaction.execute_batch(
            "ALTER TABLE display_capture_settings
                ADD COLUMN width INTEGER CHECK (width IS NULL OR width > 0);

            ALTER TABLE display_capture_settings
                ADD COLUMN height INTEGER CHECK (height IS NULL OR height > 0);

            PRAGMA user_version = 6;",
        )?;
    }
    if current_version < 7 {
        // Audio does not hang off a Scene. A microphone belongs to whoever is
        // broadcasting, and switching Scenes must not cut it — so these are
        // their own rows rather than `sources` reached through `scene_items`.
        //
        // The two everyone has are seeded, the way a first run already gets
        // "Scene 1": a mixer with nothing in it teaches the reader nothing,
        // and these are the two entries an audio mixer is expected to open
        // with. `device` is null, meaning whichever device the system calls
        // its default — which follows the user changing it, rather than
        // pinning whatever was default the day the project was made.
        transaction.execute_batch(
            "CREATE TABLE audio_sources (
                id       INTEGER PRIMARY KEY,
                name     TEXT NOT NULL UNIQUE,
                kind     TEXT NOT NULL,
                device   TEXT,
                gain_db  REAL NOT NULL DEFAULT 0,
                muted    INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL
            );

            CREATE INDEX audio_sources_position_idx ON audio_sources(position);

            INSERT INTO audio_sources (name, kind, device, gain_db, muted, position)
            VALUES ('Desktop Audio', 'output', NULL, 0, 0, 0),
                   ('Microphone',    'input',  NULL, 0, 0, 1);

            PRAGMA user_version = 7;",
        )?;
    }
    if current_version < 8 {
        // Strokes live in a table of their own rather than a blob on the
        // settings row: a Drawing gains one per gesture and loses one per
        // undo, and a row apiece is what makes those an insert and a delete
        // instead of a rewrite of everything drawn so far.
        //
        // `points` is the pairs packed as little-endian `f32`, two per point.
        // A stroke is a few hundred of them at most and they are only ever
        // read or written whole, so a column of numbers would buy nothing for
        // the rows it would cost.
        transaction.execute_batch(
            "CREATE TABLE drawing_source_settings (
                source_id INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                width     INTEGER NOT NULL CHECK (width > 0),
                height    INTEGER NOT NULL CHECK (height > 0)
            );

            CREATE TABLE drawing_strokes (
                source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                ordinal   INTEGER NOT NULL,
                red       INTEGER NOT NULL CHECK (red BETWEEN 0 AND 255),
                green     INTEGER NOT NULL CHECK (green BETWEEN 0 AND 255),
                blue      INTEGER NOT NULL CHECK (blue BETWEEN 0 AND 255),
                alpha     INTEGER NOT NULL CHECK (alpha BETWEEN 0 AND 255),
                width     REAL NOT NULL CHECK (width > 0),
                points    BLOB NOT NULL,
                PRIMARY KEY (source_id, ordinal)
            );

            PRAGMA user_version = 8;",
        )?;
    }
    if current_version < 9 {
        // A Window Capture, shaped like a Display Capture and for the same
        // reason: two platforms that cannot produce each other's answer.
        //
        // Windows and X11 identify the window by the pair a person reads off
        // a task bar — the owning executable and the title — because the
        // handle itself is only meaningful inside the session that issued it.
        // Wayland's portal names nothing and hands back a restore token.
        //
        // The size is the window's outer size when it was picked. Nullable
        // because nothing guarantees a picker reported one, and a hint either
        // way: a window is resized by whoever is using it.
        transaction.execute_batch(
            "CREATE TABLE window_capture_settings (
                source_id     INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                target_kind   TEXT NOT NULL CHECK (target_kind IN ('window', 'portal')),
                process       TEXT,
                title         TEXT,
                restore_token TEXT,
                width         INTEGER,
                height        INTEGER,
                CHECK (
                    (target_kind = 'window'
                        AND process IS NOT NULL
                        AND title IS NOT NULL
                        AND restore_token IS NULL)
                 OR (target_kind = 'portal'
                        AND process IS NULL
                        AND title IS NULL)
                )
            );

            PRAGMA user_version = 9;",
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
        let source_tables_exist: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'sources'
                ) AND EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'color_source_settings'
                ) AND EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'display_capture_settings'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(scene_name, "Existing Scene");
        assert!(source_tables_exist);
    }

    #[test]
    fn version_four_display_capture_rows_split_into_named_and_portal_targets() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sources (
                    id            INTEGER PRIMARY KEY,
                    name          TEXT NOT NULL UNIQUE,
                    kind          TEXT NOT NULL,
                    settings_json TEXT NOT NULL DEFAULT '{}'
                );
                CREATE TABLE display_capture_settings (
                    source_id    INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                    monitor_name TEXT NOT NULL
                );
                INSERT INTO sources (id, name, kind) VALUES
                    (1, 'Display Capture', 'display_capture'),
                    (2, 'Display Capture 2', 'display_capture'),
                    (3, 'Display Capture 3', 'display_capture');
                INSERT INTO display_capture_settings (source_id, monitor_name) VALUES
                    (1, 'DP-1'),
                    (2, 'portal: 42'),
                    (3, 'portal-node: 7');
                PRAGMA user_version = 4;",
            )
            .unwrap();

        run(&mut connection).unwrap();

        let mut statement = connection
            .prepare(
                "SELECT target_kind, monitor_name, restore_token
                 FROM display_capture_settings
                 ORDER BY source_id",
            )
            .unwrap();
        let rows: Vec<(String, Option<String>, Option<String>)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        // A real display name survives untouched; both placeholder spellings
        // become portal targets that have to prompt again, since a stream id
        // was never something a later session could reopen.
        assert_eq!(
            rows,
            vec![
                ("monitor".to_owned(), Some("DP-1".to_owned()), None),
                ("portal".to_owned(), None, None),
                ("portal".to_owned(), None, None),
            ]
        );
    }

    #[test]
    fn display_capture_target_kind_and_columns_must_agree() {
        let mut connection = Connection::open_in_memory().unwrap();
        run(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO sources (id, name, kind)
                 VALUES (1, 'Display Capture', 'display_capture')",
                [],
            )
            .unwrap();

        // A portal row carrying a display name is the version-4 mistake the
        // rebuild exists to prevent, so the schema itself has to reject it.
        assert!(
            connection
                .execute(
                    "INSERT INTO display_capture_settings
                        (source_id, target_kind, monitor_name)
                     VALUES (1, 'portal', 'DP-1')",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn version_five_rows_keep_working_without_a_size() {
        let mut connection = Connection::open_in_memory().unwrap();
        run(&mut connection).unwrap();
        connection
            .execute_batch(
                "INSERT INTO sources (id, name, kind)
                 VALUES (1, 'Display Capture', 'display_capture');
                 INSERT INTO display_capture_settings
                    (source_id, target_kind, monitor_name)
                 VALUES (1, 'monitor', 'DP-1');",
            )
            .unwrap();

        // A row written before the size columns existed reads back as "no
        // hint", which is what makes it fall back to Canvas size.
        let size: (Option<i64>, Option<i64>) = connection
            .query_row(
                "SELECT width, height FROM display_capture_settings WHERE source_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(size, (None, None));

        // A zero or negative size is not a missing size, so the column rejects
        // it rather than letting it become a degenerate rectangle.
        assert!(
            connection
                .execute(
                    "UPDATE display_capture_settings SET width = 0 WHERE source_id = 1",
                    [],
                )
                .is_err()
        );
    }
}
