use rusqlite::Connection;

use super::database::PersistenceResult;

const SCHEMA_VERSION: i64 = 22;

/// The schema obs-rs 0.1.0 shipped, and the oldest one that can still be
/// opened.
///
/// Everything below it used to be reachable one step at a time, seventeen
/// blocks of them. They were collapsed into [`BASELINE`] once 0.1.0 was the
/// first release anybody but the author had run: the only databases that
/// ever carried a lower number were made by builds from this tree before
/// then.
const BASELINE_VERSION: i64 = 17;

/// The whole schema as of [`BASELINE_VERSION`], verbatim.
///
/// Copied out of `sqlite_master` rather than rewritten, which is why some of
/// it looks the way it does — `display_capture_settings` is quoted because
/// version 4 rebuilt and renamed it, and several tables carry columns
/// appended after their closing parenthesis because a later version added
/// them. Tidying that up would have produced a schema that *looks* like the
/// one in the field without any way left to prove it is the same one: a
/// `CHECK` constraint is not something a pragma can be asked about, so a
/// dropped one would only surface as a row that should have been refused.
///
/// So this is not written, it is recorded.
const BASELINE: &str = r#"
CREATE TABLE app_state (
                id                INTEGER PRIMARY KEY CHECK (id = 1),
                selected_scene_id INTEGER REFERENCES scenes(id) ON DELETE SET NULL
            );

CREATE TABLE audio_sources (
                id       INTEGER PRIMARY KEY,
                name     TEXT NOT NULL UNIQUE,
                kind     TEXT NOT NULL,
                device   TEXT,
                gain_db  REAL NOT NULL DEFAULT 0,
                muted    INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL
            , monitored INTEGER NOT NULL DEFAULT 0
                CHECK (monitored IN (0, 1)));

CREATE TABLE color_source_settings (
                source_id INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                width     INTEGER NOT NULL,
                height    INTEGER NOT NULL,
                red       INTEGER NOT NULL CHECK (red BETWEEN 0 AND 255),
                green     INTEGER NOT NULL CHECK (green BETWEEN 0 AND 255),
                blue      INTEGER NOT NULL CHECK (blue BETWEEN 0 AND 255),
                alpha     INTEGER NOT NULL CHECK (alpha BETWEEN 0 AND 255)
            );

CREATE TABLE "display_capture_settings" (
                source_id     INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                target_kind   TEXT NOT NULL CHECK (target_kind IN ('monitor', 'portal')),
                monitor_name  TEXT,
                restore_token TEXT, width INTEGER CHECK (width IS NULL OR width > 0), height INTEGER CHECK (height IS NULL OR height > 0),
                CHECK (
                    (target_kind = 'monitor'
                        AND monitor_name IS NOT NULL
                        AND restore_token IS NULL)
                 OR (target_kind = 'portal' AND monitor_name IS NULL)
                )
            );

CREATE TABLE drawing_source_settings (
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

CREATE TABLE image_source_settings (
                source_id INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                path      TEXT NOT NULL,
                width     INTEGER,
                height    INTEGER
            );

CREATE TABLE media_file_settings (
                source_id INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                path      TEXT NOT NULL,
                looping   INTEGER NOT NULL CHECK (looping IN (0, 1)),
                width     INTEGER,
                height    INTEGER,
                has_audio INTEGER NOT NULL CHECK (has_audio IN (0, 1)),
                gain_db   REAL NOT NULL,
                muted     INTEGER NOT NULL CHECK (muted IN (0, 1))
            , duration_us INTEGER, paused INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)), monitored INTEGER NOT NULL DEFAULT 0
                CHECK (monitored IN (0, 1)));

CREATE TABLE rtsp_source_settings (
                source_id         INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                url               TEXT NOT NULL,
                transport         TEXT NOT NULL CHECK (transport IN ('tcp', 'udp')),
                reconnect_seconds INTEGER CHECK (reconnect_seconds > 0),
                width             INTEGER,
                height            INTEGER,
                has_audio         INTEGER NOT NULL CHECK (has_audio IN (0, 1)),
                gain_db           REAL NOT NULL,
                muted             INTEGER NOT NULL CHECK (muted IN (0, 1))
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

CREATE TABLE scenes (
                id       INTEGER PRIMARY KEY,
                name     TEXT NOT NULL UNIQUE,
                position INTEGER NOT NULL
            );

CREATE TABLE sources (
                id            INTEGER PRIMARY KEY,
                name          TEXT NOT NULL UNIQUE,
                kind          TEXT NOT NULL,
                settings_json TEXT NOT NULL DEFAULT '{}'
            );

CREATE TABLE video_capture_settings (
                source_id             INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                device                TEXT NOT NULL,
                device_name           TEXT NOT NULL,
                mode_width            INTEGER,
                mode_height           INTEGER,
                mode_rate_numerator   INTEGER,
                mode_rate_denominator INTEGER,
                width                 INTEGER,
                height                INTEGER
            );

CREATE TABLE window_capture_settings (
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

CREATE INDEX audio_sources_position_idx ON audio_sources(position);

CREATE INDEX scene_items_scene_z_idx
            ON scene_items(scene_id, z_index DESC);

CREATE INDEX scene_items_source_idx
            ON scene_items(source_id);

CREATE INDEX scenes_position_idx ON scenes(position);
INSERT INTO scenes (name, position) VALUES ('Scene 1', 0);
INSERT INTO app_state (id, selected_scene_id)
VALUES (1, last_insert_rowid());

INSERT INTO audio_sources (name, kind, device, gain_db, muted, position)
VALUES ('Desktop Audio', 'output', NULL, 0, 0, 0),
       ('Microphone',    'input',  NULL, 0, 0, 1);

PRAGMA user_version = 17;

"#;

pub(super) fn run(connection: &mut Connection) -> PersistenceResult<()> {
    let current_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current_version >= SCHEMA_VERSION {
        return Ok(());
    }
    // Zero is a database that does not exist yet, which every new project
    // starts as. Anything between that and the baseline was made by a build
    // that predates the first release, and the steps that would have carried
    // it forward are gone — so it is refused by name rather than opened into
    // a schema half of it does not have.
    if current_version > 0 && current_version < BASELINE_VERSION {
        return Err(format!(
            "this project was made by a build of obs-rs from before 0.1.0 \
             (schema {current_version}); the oldest that can be opened is \
             {BASELINE_VERSION}"
        )
        .into());
    }

    let transaction = connection.transaction()?;
    if current_version == 0 {
        transaction.execute_batch(BASELINE)?;
    }
    if current_version < 18 {
        // Filters, and the first one: a chroma key.
        //
        // They hang off the Source rather than the SceneItem, which is where
        // a Transform and a Crop hang. Placing one camera differently in two
        // Scenes is the point of having it twice; keying its green screen is
        // a property of what the camera is showing, and wanting that in one
        // Scene and not another would be wanting two cameras.
        //
        // `position` orders them within a Source rather than globally: a
        // filter chain is applied in order, and the number only has to mean
        // something next to its siblings. Reordering is the same neighbour
        // swap `scene_items.z_index` already gets.
        //
        // The settings live in their own table, keyed by the filter rather
        // than by the source, which is the same shape `sources` and its
        // per-kind settings tables already have one level up. A second kind
        // of filter is then a table and a `kind` value, not a column added
        // to a shared one.
        transaction.execute_batch(
            "CREATE TABLE source_filters (
                id        INTEGER PRIMARY KEY,
                source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                position  INTEGER NOT NULL,
                kind      TEXT NOT NULL,
                enabled   INTEGER NOT NULL DEFAULT 1
            );

            CREATE INDEX source_filters_source_idx
                ON source_filters(source_id, position);

            CREATE TABLE chroma_key_filter_settings (
                filter_id  INTEGER PRIMARY KEY
                           REFERENCES source_filters(id) ON DELETE CASCADE,
                method     TEXT NOT NULL
                           CHECK (method IN ('green', 'blue', 'custom')),
                custom_red   INTEGER NOT NULL,
                custom_green INTEGER NOT NULL,
                custom_blue  INTEGER NOT NULL,
                threshold  REAL NOT NULL,
                smoothing  REAL NOT NULL
            );

            PRAGMA user_version = 18;",
        )?;
    }
    if current_version < 19 {
        // A Text Source.
        //
        // `width`/`height` are the box glyphs are drawn into rather than
        // anything the text measures out to: the surface is fixed when the
        // Source opens, and a string whose width changed on every tick would
        // otherwise reopen the pipeline once a second. `alignment` is what
        // decides which edge stays put as the string underneath it changes
        // length.
        //
        // `font` is a path and is nullable, which means "the font this
        // application already found for its own interface" rather than "no
        // font" — see `crate::i18n::font`. Storing the bytes would put a
        // licence question inside the project file for no gain, since the
        // file is the same one each time the project opens.
        transaction.execute_batch(
            "CREATE TABLE text_source_settings (
                source_id INTEGER PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                width     INTEGER NOT NULL CHECK (width > 0),
                height    INTEGER NOT NULL CHECK (height > 0),
                text      TEXT NOT NULL,
                font      TEXT,
                font_size REAL NOT NULL CHECK (font_size > 0),
                red       INTEGER NOT NULL CHECK (red BETWEEN 0 AND 255),
                green     INTEGER NOT NULL CHECK (green BETWEEN 0 AND 255),
                blue      INTEGER NOT NULL CHECK (blue BETWEEN 0 AND 255),
                alpha     INTEGER NOT NULL CHECK (alpha BETWEEN 0 AND 255),
                alignment TEXT NOT NULL
                          CHECK (alignment IN ('left', 'centre', 'right'))
            );

            PRAGMA user_version = 19;",
        )?;
    }
    if current_version < 20 {
        // What a Text Source says, as opposed to how it looks: a typed line,
        // the wall clock, or a stopwatch of its own.
        //
        // `text` is left alone rather than doubling as the clock's format.
        // One column per mode's settings means switching to a clock and back
        // returns what was typed instead of having eaten it — and a format
        // is a choice from a list here, not a string, because a mistyped
        // format description produces a blank caption and there is nowhere
        // to report that: the caption *is* where a Source reports.
        //
        // The stopwatch is two columns for the reason a stopwatch has two
        // hands: `timer_running_since` is the run in progress and
        // `timer_accumulated_us` is every run before it. The first is unix
        // microseconds rather than anything monotonic because it is written
        // to this file and read back on a later launch, where a monotonic
        // clock means nothing.
        transaction.execute_batch(
            "ALTER TABLE text_source_settings
                ADD COLUMN mode TEXT NOT NULL DEFAULT 'static'
                    CHECK (mode IN ('static', 'clock', 'timer'));
             ALTER TABLE text_source_settings
                ADD COLUMN clock_format TEXT NOT NULL DEFAULT 'time';
             ALTER TABLE text_source_settings
                ADD COLUMN timer_format TEXT NOT NULL DEFAULT 'hours-minutes-seconds';
             ALTER TABLE text_source_settings
                ADD COLUMN timer_running_since INTEGER;
             ALTER TABLE text_source_settings
                ADD COLUMN timer_accumulated_us INTEGER NOT NULL DEFAULT 0
                    CHECK (timer_accumulated_us >= 0);

            PRAGMA user_version = 20;",
        )?;
    }
    if current_version < 21 {
        // Clears a flag nothing can clear any more.
        //
        // Monitoring was offered on every channel for a while before Desktop
        // Audio lost the control, and a project that switched it on in
        // between kept the value with no button left to switch it off. The
        // engine no longer acts on it either — see `source_monitors` — so
        // this is for whatever reads the column next: a flag that says
        // something the application refuses to do is a trap for the first
        // person who takes it at its word.
        transaction.execute_batch(
            "UPDATE audio_sources SET monitored = 0 WHERE kind = 'output';

            PRAGMA user_version = 21;",
        )?;
    }
    if current_version < 22 {
        // Filters on the mixer's channels: noise suppression and a gate.
        //
        // A table of their own rather than rows in `source_filters`, because
        // what they hang off is not a Source. A mixer channel is an
        // `audio_sources` row, which is in no Scene, and a filter row that
        // could point at either would need two nullable owners and a check
        // that exactly one is set — and would let a command meant for one
        // kind of filter land on the other, since the ids would share one
        // sequence. The shape is otherwise the one `source_filters` has, and
        // for its reasons: ordered within an owner, a settings table per
        // kind that has any.
        //
        // Noise suppression has no settings, so it has no table.
        transaction.execute_batch(
            "CREATE TABLE audio_source_filters (
                id              INTEGER PRIMARY KEY,
                audio_source_id INTEGER NOT NULL
                                REFERENCES audio_sources(id) ON DELETE CASCADE,
                position        INTEGER NOT NULL,
                kind            TEXT NOT NULL,
                enabled         INTEGER NOT NULL DEFAULT 1
            );

            CREATE INDEX audio_source_filters_source_idx
                ON audio_source_filters(audio_source_id, position);

            CREATE TABLE noise_gate_filter_settings (
                filter_id          INTEGER PRIMARY KEY
                                   REFERENCES audio_source_filters(id) ON DELETE CASCADE,
                open_threshold_db  REAL NOT NULL,
                close_threshold_db REAL NOT NULL,
                attack_ms          INTEGER NOT NULL CHECK (attack_ms >= 0),
                hold_ms            INTEGER NOT NULL CHECK (hold_ms >= 0),
                release_ms         INTEGER NOT NULL CHECK (release_ms >= 0)
            );

            PRAGMA user_version = 22;",
        )?;
    }
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_project_comes_up_at_the_current_schema() {
        let mut connection = Connection::open_in_memory().unwrap();
        run(&mut connection).unwrap();

        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let scene_name: String = connection
            .query_row("SELECT name FROM scenes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(scene_name, "Scene 1", "a project opens with a Scene");
    }

    /// What every database in the field is: the baseline alone, exactly as
    /// 0.1.0 left it, with nothing after it yet.
    #[test]
    fn a_project_from_the_first_release_is_carried_forward() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(BASELINE).unwrap();
        assert_eq!(
            connection
                .query_row::<i64, _, _>("PRAGMA user_version", [], |row| row.get(0))
                .unwrap(),
            BASELINE_VERSION,
            "the baseline is what 0.1.0 shipped"
        );

        run(&mut connection).unwrap();

        assert_eq!(
            connection
                .query_row::<i64, _, _>("PRAGMA user_version", [], |row| row.get(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        // The Scene it already had is still there, and the tables it did not
        // have now are.
        assert_eq!(
            connection
                .query_row::<i64, _, _>("SELECT COUNT(*) FROM scenes", [], |row| row.get(0))
                .unwrap(),
            1
        );
        for table in [
            "source_filters",
            "chroma_key_filter_settings",
            "audio_source_filters",
            "noise_gate_filter_settings",
        ] {
            assert_eq!(
                connection
                    .query_row::<i64, _, _>(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                        [table],
                        |row| row.get(0)
                    )
                    .unwrap(),
                1,
                "{table} should have been added"
            );
        }
    }

    /// The cost of collapsing seventeen steps into one, said out loud: a
    /// database from before the first release cannot be carried forward, and
    /// is refused by name rather than opened into a schema half of it does
    /// not have.
    #[test]
    fn a_project_older_than_the_first_release_is_refused() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE scenes (
                    id       INTEGER PRIMARY KEY,
                    name     TEXT NOT NULL UNIQUE,
                    position INTEGER NOT NULL
                );
                PRAGMA user_version = 1;",
            )
            .unwrap();

        let error = run(&mut connection).expect_err("a pre-release schema cannot be upgraded");
        let message = error.to_string();
        assert!(
            message.contains("0.1.0") && message.contains("schema 1"),
            "the message has to say which version made it: {message}"
        );

        assert_eq!(
            connection
                .query_row::<i64, _, _>("PRAGMA user_version", [], |row| row.get(0))
                .unwrap(),
            1,
            "and it must be left as it was found, not half-migrated"
        );
    }

    /// Sources that existed before monitoring did have never been played
    /// back, and `off` is the mode that says so. Anything else would start
    /// somebody's microphone talking into their own speakers on the first
    /// launch after an update.
    #[test]
    fn audio_sources_that_predate_monitoring_come_back_not_monitored() {
        let mut connection = Connection::open_in_memory().unwrap();
        run(&mut connection).unwrap();

        let monitored: Vec<i64> = connection
            .prepare("SELECT monitored FROM audio_sources ORDER BY position")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(!monitored.is_empty(), "the schema ships two audio sources");
        assert!(monitored.iter().all(|on| *on == 0), "got {monitored:?}");
    }

    /// A project saved while Desktop Audio could still be monitored comes
    /// back with that cleared — and only that: a monitored microphone is a
    /// choice somebody made and can still see.
    #[test]
    fn a_monitored_desktop_is_cleared_and_a_monitored_microphone_is_kept() {
        let mut connection = Connection::open_in_memory().unwrap();
        run(&mut connection).unwrap();
        // Back to 20 — taking with it what the later steps added, so running
        // again carries the database forward from 20 rather than tripping
        // over tables a project at 20 would not have.
        connection
            .execute_batch(
                "UPDATE audio_sources SET monitored = 1;
                 DROP TABLE noise_gate_filter_settings;
                 DROP TABLE audio_source_filters;
                 PRAGMA user_version = 20;",
            )
            .unwrap();

        run(&mut connection).unwrap();

        let monitored = |kind: &str| -> i64 {
            connection
                .query_row(
                    "SELECT monitored FROM audio_sources WHERE kind = ?1",
                    [kind],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(monitored("output"), 0, "Desktop Audio is not monitored");
        assert_eq!(monitored("input"), 1, "the microphone keeps its choice");
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
