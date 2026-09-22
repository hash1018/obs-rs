use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::session::{Changeset, ConflictAction, ConflictType, Session};
use rusqlite::{Connection, Transaction};

use super::migrations;

pub(crate) type PersistenceResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub(crate) struct ProjectDatabase {
    connection: Connection,
}

impl ProjectDatabase {
    pub(crate) fn open_default() -> PersistenceResult<Self> {
        Self::open(default_database_path())
    }

    pub(crate) fn open(path: impl AsRef<Path>) -> PersistenceResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut connection = Connection::open(path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        migrations::run(&mut connection)?;

        Ok(Self { connection })
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> PersistenceResult<Self> {
        let mut connection = Connection::open_in_memory()?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrations::run(&mut connection)?;
        Ok(Self { connection })
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(crate) fn transaction<T>(
        &mut self,
        operation: impl FnOnce(&Transaction<'_>) -> PersistenceResult<T>,
    ) -> PersistenceResult<T> {
        let transaction = self.connection.transaction()?;
        let result = operation(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    /// The same, recording what it changed.
    ///
    /// What Edit → Undo is built on. SQLite's session extension watches every
    /// table while `operation` runs and hands back the rows it inserted,
    /// updated and deleted — including those a foreign key's `ON DELETE
    /// CASCADE` took with them, which is what lets deleting a Source be
    /// undone with its filters and placements intact. `None` where the
    /// operation changed nothing, so a no-op is never a step to undo.
    ///
    /// Only what `operation` did: a write that reaches the database any other
    /// way is outside the session and outside what an undo can touch.
    pub(crate) fn recorded_transaction<T>(
        &mut self,
        operation: impl FnOnce(&Transaction<'_>) -> PersistenceResult<T>,
    ) -> PersistenceResult<(T, Option<Changeset>)> {
        let transaction = self.connection.transaction()?;
        let (result, changes) = {
            let mut session = Session::new(&transaction)?;
            session.attach(None::<&str>)?;
            let result = operation(&transaction)?;
            let changes = if session.is_empty() {
                None
            } else {
                Some(session.changeset()?)
            };
            (result, changes)
        };
        transaction.commit()?;
        Ok((result, changes))
    }

    /// Applies a recorded change — an undo's inverse, or a redo's original —
    /// and then `after`, all in one transaction.
    ///
    /// A row that has moved on since the change was recorded is resolved in
    /// the change's favour: an undo is somebody asking for exactly what was
    /// there before, and a changeset only names the columns it changed, so
    /// this overrides nothing the step did not touch. A row that has gone is
    /// left gone.
    pub(crate) fn apply_recorded(
        &mut self,
        changes: &Changeset,
        after: impl FnOnce(&Transaction<'_>) -> PersistenceResult<()>,
    ) -> PersistenceResult<()> {
        let transaction = self.connection.transaction()?;
        transaction.apply(
            changes,
            None::<fn(&str) -> bool>,
            |conflict, _item| match conflict {
                ConflictType::SQLITE_CHANGESET_DATA | ConflictType::SQLITE_CHANGESET_CONFLICT => {
                    ConflictAction::SQLITE_CHANGESET_REPLACE
                }
                _ => ConflictAction::SQLITE_CHANGESET_OMIT,
            },
        )?;
        after(&transaction)?;
        transaction.commit()?;
        Ok(())
    }
}

fn default_database_path() -> PathBuf {
    crate::paths::data_dir().join("project.db")
}
