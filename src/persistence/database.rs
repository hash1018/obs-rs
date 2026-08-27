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
        Self::open(default_database_path()?)
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

fn default_database_path() -> PersistenceResult<PathBuf> {
    let directory = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|path| path.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|path| path.join(".local/share"))
            })
    }
    .unwrap_or(std::env::current_dir()?);

    Ok(directory.join("obs-rs").join("project.db"))
}
