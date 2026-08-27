use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
}

fn default_database_path() -> PathBuf {
    crate::paths::data_dir().join("project.db")
}
