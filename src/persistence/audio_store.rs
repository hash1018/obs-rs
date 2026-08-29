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
            "SELECT id, name, kind, device, gain_db, muted
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
                Ok(
                    AudioSourceKind::from_storage_name(&stored_kind).map(|kind| AudioSource {
                        id,
                        name,
                        kind,
                        device,
                        gain_db,
                        muted: muted != 0,
                    }),
                )
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().flatten().collect())
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
        assert_eq!(AudioSourceKind::from_storage_name("midi"), None);
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
