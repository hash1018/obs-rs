use rusqlite::{Connection, Transaction, params};

use crate::domain::{AudioSource, AudioSourceId, AudioSourceKind, MAX_GAIN_DB, MIN_GAIN_DB};

use super::PersistenceResult;

pub(crate) struct AudioStore;

impl AudioStore {
    /// Every audio source, in the order the mixer shows them.
    ///
    /// A row whose `kind` is not one this build knows is skipped rather than
    /// failing the read: a project written by a later version must still
    /// open, showing what it understands.
    pub(crate) fn list(connection: &Connection) -> PersistenceResult<Vec<AudioSource>> {
        let mut statement = connection.prepare(
            "SELECT id, name, kind, device, gain_db, muted, monitored
             FROM audio_sources
             ORDER BY position, id",
        )?;
        let rows = statement
            .query_map([], |row| {
                let stored_kind: String = row.get(2)?;
                let id = AudioSourceId(row.get(0)?);
                let name: String = row.get(1)?;
                let device: Option<String> = row.get(3)?;
                let gain_db: f32 = row.get(4)?;
                let muted: i64 = row.get(5)?;
                let monitored: i64 = row.get(6)?;
                Ok(
                    AudioSourceKind::from_storage_name(&stored_kind).map(|kind| AudioSource {
                        id,
                        name,
                        kind,
                        device,
                        gain_db,
                        muted: muted != 0,
                        monitored: monitored != 0,
                    }),
                )
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().flatten().collect())
    }

    /// Adds a channel that captures one application's sound, listening to
    /// nothing until one is picked.
    ///
    /// The only kind of channel a project gains or loses: the desktop and
    /// the microphone are there from the first run, because every machine
    /// has both sides of a sound card, and an application channel is one
    /// somebody asked for.
    ///
    /// Named `Application Audio`, and `Application Audio 2` where that is
    /// taken — `audio_sources.name` is UNIQUE, and it is also the name the
    /// engine registers this channel's pipeline under. The name is a
    /// fallback for the dock, which shows the chosen application instead as
    /// soon as there is one.
    pub(crate) fn add_application(
        transaction: &Transaction<'_>,
    ) -> PersistenceResult<AudioSourceId> {
        let name = free_name(transaction)?;
        let position: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM audio_sources",
            [],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO audio_sources (name, kind, device, gain_db, muted, position)
             VALUES (?1, 'application', NULL, 0, 0, ?2)",
            params![name, position],
        )?;
        Ok(AudioSourceId(transaction.last_insert_rowid()))
    }

    /// Takes a channel away, with the filters on it.
    ///
    /// Only an application channel: the two device channels are what the
    /// mixer *is*, and a project that had lost one would have no way back to
    /// it. Refused rather than ignored, so a caller that meant one cannot
    /// quietly remove the other.
    pub(crate) fn remove(
        transaction: &Transaction<'_>,
        id: AudioSourceId,
    ) -> PersistenceResult<()> {
        let kind: Option<String> = transaction
            .query_row(
                "SELECT kind FROM audio_sources WHERE id = ?1",
                params![id.0],
                |row| row.get(0),
            )
            .ok();
        if kind.as_deref() != Some("application") {
            return Err("only an application channel can be removed".into());
        }
        // The filters go with it through `ON DELETE CASCADE`; see the audio
        // filter table's own foreign key.
        transaction.execute("DELETE FROM audio_sources WHERE id = ?1", params![id.0])?;
        Ok(())
    }

    pub(crate) fn set_monitored(
        transaction: &Transaction<'_>,
        id: AudioSourceId,
        monitored: bool,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE audio_sources SET monitored = ?2 WHERE id = ?1",
            params![id.0, monitored],
        )?;
        Ok(())
    }

    /// Clamped here rather than trusted from the caller: a fader is one way
    /// in, and a value from anywhere else must not be able to store a gain
    /// the mixer cannot show or a device cannot apply.
    pub(crate) fn set_gain_db(
        transaction: &Transaction<'_>,
        id: AudioSourceId,
        gain_db: f32,
    ) -> PersistenceResult<()> {
        let gain_db = if gain_db.is_finite() {
            gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
        } else {
            0.0
        };
        transaction.execute(
            "UPDATE audio_sources SET gain_db = ?2 WHERE id = ?1",
            params![id.0, gain_db],
        )?;
        Ok(())
    }

    /// `None` is not "unset": it is the instruction to follow whichever
    /// device the system calls its default, so it keeps working when that
    /// changes.
    pub(crate) fn set_device(
        transaction: &Transaction<'_>,
        id: AudioSourceId,
        device: Option<&str>,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE audio_sources SET device = ?2 WHERE id = ?1",
            params![id.0, device],
        )?;
        Ok(())
    }

    pub(crate) fn set_muted(
        transaction: &Transaction<'_>,
        id: AudioSourceId,
        muted: bool,
    ) -> PersistenceResult<()> {
        transaction.execute(
            "UPDATE audio_sources SET muted = ?2 WHERE id = ?1",
            params![id.0, i64::from(muted)],
        )?;
        Ok(())
    }
}

/// A name no channel has yet, for a new application channel.
///
/// Counts up rather than using the id, which is not known until the row is
/// written — and a name that came from an id would leave gaps a person can
/// see ("Application Audio 7" in a project with two of them).
fn free_name(transaction: &Transaction<'_>) -> PersistenceResult<String> {
    const BASE: &str = "Application Audio";
    for suffix in 1..=u32::MAX {
        let name = if suffix == 1 {
            BASE.to_owned()
        } else {
            format!("{BASE} {suffix}")
        };
        let taken: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM audio_sources WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        if taken == 0 {
            return Ok(name);
        }
    }
    Err("every application channel name is taken".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::ProjectDatabase;

    /// The migration seeds the two entries a mixer opens with, and nothing
    /// else proves they come back out as the kinds they went in as.
    #[test]
    fn a_new_project_opens_with_a_desktop_and_a_microphone() {
        let database = ProjectDatabase::open_in_memory().unwrap();

        let sources = AudioStore::list(database.connection()).unwrap();

        let described: Vec<_> = sources
            .iter()
            .map(|source| (source.name.as_str(), source.kind, source.device.as_deref()))
            .collect();
        assert_eq!(
            described,
            vec![
                ("Desktop Audio", AudioSourceKind::Output, None),
                ("Microphone", AudioSourceKind::Input, None),
            ],
            "no device means whichever the system calls its default, not unset"
        );
        assert!(sources.iter().all(|source| source.gain_db == 0.0));
        assert!(sources.iter().all(|source| !source.muted));
    }

    /// The names are the ones migration 7 writes into the column. Changing
    /// one without the other would not fail to compile — it would drop every
    /// row on read, because `list` skips a kind it does not recognise.
    #[test]
    fn the_kinds_the_migration_seeds_are_the_ones_that_read_back() {
        assert_eq!(
            AudioSourceKind::from_storage_name("output"),
            Some(AudioSourceKind::Output)
        );
        assert_eq!(
            AudioSourceKind::from_storage_name("input"),
            Some(AudioSourceKind::Input)
        );
        assert_eq!(
            AudioSourceKind::from_storage_name("application"),
            Some(AudioSourceKind::Application)
        );
        assert_eq!(AudioSourceKind::from_storage_name("midi"), None);
    }

    /// An application channel is the one a project gains and loses. It comes
    /// back out as the kind it went in as, listening to nothing until an
    /// application is chosen, and the second one is not named the same as
    /// the first — the column is UNIQUE, and the engine registers this
    /// channel's pipeline under that name.
    #[test]
    fn application_channels_are_added_named_and_taken_away() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();

        let first = database
            .transaction(AudioStore::add_application)
            .expect("a channel is added");
        let _second = database
            .transaction(AudioStore::add_application)
            .expect("and another");

        let sources = AudioStore::list(database.connection()).unwrap();
        let added: Vec<_> = sources
            .iter()
            .filter(|source| source.kind == AudioSourceKind::Application)
            .map(|source| (source.name.as_str(), source.device.as_deref()))
            .collect();
        assert_eq!(
            added,
            vec![("Application Audio", None), ("Application Audio 2", None)],
            "listening to nothing until one is picked"
        );

        database
            .transaction(|transaction| AudioStore::remove(transaction, first))
            .expect("and removed again");
        let names: Vec<String> = AudioStore::list(database.connection())
            .unwrap()
            .into_iter()
            .map(|source| source.name)
            .collect();
        assert_eq!(
            names,
            vec![
                "Desktop Audio".to_owned(),
                "Microphone".to_owned(),
                "Application Audio 2".to_owned()
            ]
        );
    }

    /// The desktop and the microphone are what the mixer *is*: a project
    /// that had lost one would have no way back to it, so removing one is
    /// refused rather than quietly done.
    #[test]
    fn a_device_channel_cannot_be_removed() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let desktop = AudioStore::list(database.connection()).unwrap()[0].id;

        assert!(
            database
                .transaction(|transaction| AudioStore::remove(transaction, desktop))
                .is_err()
        );
        assert_eq!(AudioStore::list(database.connection()).unwrap().len(), 2);
    }

    #[test]
    fn a_fader_cannot_store_a_gain_the_mixer_could_not_show() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let id = AudioStore::list(database.connection()).unwrap()[0].id;

        for (asked, stored) in [
            (-90.0, MIN_GAIN_DB),
            (100.0, MAX_GAIN_DB),
            (f32::NAN, 0.0),
            (-12.5, -12.5),
            // A boost is stored as asked rather than pulled back to unity,
            // which is what the fader stopping there used to make it.
            (6.0, 6.0),
        ] {
            database
                .transaction(|transaction| AudioStore::set_gain_db(transaction, id, asked))
                .unwrap();
            let gain = AudioStore::list(database.connection()).unwrap()[0].gain_db;
            assert_eq!(gain, stored, "asked for {asked}");
        }
    }

    #[test]
    fn muting_is_remembered() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let id = AudioStore::list(database.connection()).unwrap()[0].id;

        database
            .transaction(|transaction| AudioStore::set_muted(transaction, id, true))
            .unwrap();

        assert!(AudioStore::list(database.connection()).unwrap()[0].muted);
    }
}
