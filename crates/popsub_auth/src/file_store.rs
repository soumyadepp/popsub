//! `FileUserStore`, a `UserStore` that keeps accounts in one JSON file so
//! they survive restarts.
//!
//! The whole store is read into memory when it opens and rewritten on every
//! change: written to a temporary file next to it, flushed to disk, then
//! renamed over the old one, so a crash leaves either the old file or the
//! new one and never a half-written one. A change is only applied in memory
//! once the file is written, so memory and disk never disagree. On Unix the
//! file is created readable and writable by its owner only, since it holds
//! password hashes.
//!
//! Account changes are rare (sign-ups, role changes), so rewriting the file
//! each time is cheap. The writes are synchronous and small.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{AuthError, Result};
use crate::store::UserStore;
use crate::user::User;

/// The format version written to the file, checked when it is read.
const FORMAT_VERSION: u32 = 1;

/// What the file holds.
#[derive(Debug, Serialize, Deserialize)]
struct UsersFile {
    /// The format version.
    version: u32,
    /// Every account, sorted by username.
    users: Vec<User>,
}

/// A `UserStore` kept in a JSON file.
#[derive(Debug)]
pub struct FileUserStore {
    /// The file.
    path: PathBuf,
    /// Every account, by username.
    users: Mutex<BTreeMap<String, User>>,
    /// Counts the changes made to `users`, for `UserStore::revision`.
    revision: AtomicU64,
}

impl FileUserStore {
    /// Opens the store, reading the file if it exists. A missing file is an
    /// empty store; the file is created on the first change.
    ///
    /// Args:
    ///     path: The JSON file, usually `users.json` in the data directory.
    ///
    /// Returns:
    ///     The store.
    ///
    /// Raises:
    ///     AuthError::Internal: When the file exists but can't be read or
    ///         parsed, or was written by a newer format version.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let users = match fs::read(&path) {
            Ok(bytes) => {
                let file: UsersFile = serde_json::from_slice(&bytes).map_err(|e| {
                    AuthError::Internal(format!("{} can't be parsed: {e}", path.display()))
                })?;
                if file.version != FORMAT_VERSION {
                    return Err(AuthError::Internal(format!(
                        "{} has format version {}, this server reads {FORMAT_VERSION}",
                        path.display(),
                        file.version
                    )));
                }
                file.users
                    .into_iter()
                    .map(|user| (user.username.clone(), user))
                    .collect()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                return Err(AuthError::Internal(format!(
                    "can't read {}: {e}",
                    path.display()
                )));
            }
        };
        Ok(Self {
            path,
            users: Mutex::new(users),
            revision: AtomicU64::new(0),
        })
    }

    /// The file the store lives in.
    ///
    /// Returns:
    ///     Its path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Locks the accounts, recovering them if a holder panicked. Every change
    /// is applied only after the file is written, so the map is always
    /// consistent with the file.
    ///
    /// Returns:
    ///     The guard.
    fn users(&self) -> MutexGuard<'_, BTreeMap<String, User>> {
        self.users.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Applies a change: works it out on a copy, writes the copy to the
    /// file, and only then replaces the accounts in memory.
    ///
    /// Args:
    ///     change: Edits the copy and returns the call's result, or an error
    ///         to leave everything as it was.
    ///
    /// Returns:
    ///     What `change` returned.
    ///
    /// Raises:
    ///     AuthError: Whatever `change` raised, or `AuthError::Internal` when
    ///         the file can't be written.
    fn change<T>(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, User>) -> Result<T>,
    ) -> Result<T> {
        let mut users = self.users();
        let mut next = users.clone();
        let result = change(&mut next)?;
        self.write(&next)?;
        *users = next;
        self.revision.fetch_add(1, Ordering::Release);
        Ok(result)
    }

    /// Writes the accounts to the file atomically.
    ///
    /// Args:
    ///     users: The accounts to write.
    ///
    /// Raises:
    ///     AuthError::Internal: When any step of writing fails.
    fn write(&self, users: &BTreeMap<String, User>) -> Result<()> {
        let internal = |what: &str, e: std::io::Error| {
            AuthError::Internal(format!("can't {what} {}: {e}", self.path.display()))
        };
        let file = UsersFile {
            version: FORMAT_VERSION,
            users: users.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file)
            .map_err(|e| AuthError::Internal(format!("can't encode the users: {e}")))?;
        let temp = self.path.with_extension("json.tmp");
        let mut out =
            create_private(&temp).map_err(|e| internal("create a temporary file for", e))?;
        out.write_all(&bytes).map_err(|e| internal("write", e))?;
        out.sync_all().map_err(|e| internal("flush", e))?;
        fs::rename(&temp, &self.path).map_err(|e| internal("replace", e))?;
        if let Some(dir) = self.path.parent()
            && let Ok(dir) = File::open(dir)
        {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

/// Creates (or truncates) a file only its owner can read and write.
///
/// Args:
///     path: The file.
///
/// Returns:
///     The open file.
///
/// Raises:
///     std::io::Error: When it can't be created.
fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[async_trait]
impl UserStore for FileUserStore {
    /// Reads an account from memory.
    ///
    /// Args:
    ///     username: The account.
    ///
    /// Returns:
    ///     The user, or `None`.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn get_user(&self, username: &str) -> Result<Option<User>> {
        Ok(self.users().get(username).cloned())
    }

    /// Adds an account and writes the file.
    ///
    /// Args:
    ///     user: The account.
    ///
    /// Raises:
    ///     AuthError::UserAlreadyExists: When the username is taken.
    ///     AuthError::Internal: When the file can't be written.
    async fn add_user(&self, user: User) -> Result<()> {
        self.change(|users| {
            if users.contains_key(&user.username) {
                return Err(AuthError::UserAlreadyExists(user.username));
            }
            users.insert(user.username.clone(), user);
            Ok(())
        })
    }

    /// Replaces an account and writes the file.
    ///
    /// Args:
    ///     user: The new state of the account.
    ///
    /// Raises:
    ///     AuthError::UserNotFound: When there is no such account.
    ///     AuthError::Internal: When the file can't be written.
    async fn update_user(&self, user: User) -> Result<()> {
        self.change(|users| match users.get_mut(&user.username) {
            Some(existing) => {
                *existing = user;
                Ok(())
            }
            None => Err(AuthError::UserNotFound(user.username)),
        })
    }

    /// Removes an account and writes the file, if it existed.
    ///
    /// Args:
    ///     username: The account.
    ///
    /// Returns:
    ///     Whether it existed.
    ///
    /// Raises:
    ///     AuthError::Internal: When the file can't be written.
    async fn remove_user(&self, username: &str) -> Result<bool> {
        if !self.users().contains_key(username) {
            return Ok(false);
        }
        self.change(|users| Ok(users.remove(username).is_some()))
    }

    /// Reports whether an account exists.
    ///
    /// Args:
    ///     username: The username.
    ///
    /// Returns:
    ///     Whether it exists.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn user_exists(&self, username: &str) -> Result<bool> {
        Ok(self.users().contains_key(username))
    }

    /// Lists every username, sorted.
    ///
    /// Returns:
    ///     The usernames.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn list_usernames(&self) -> Result<Vec<String>> {
        Ok(self.users().keys().cloned().collect())
    }

    /// Counts the accounts.
    ///
    /// Returns:
    ///     The count.
    ///
    /// Raises:
    ///     Never returns an error.
    async fn user_count(&self) -> Result<usize> {
        Ok(self.users().len())
    }

    /// Counts the changes made since the store was opened. The file is only
    /// read at start, so nothing changes the accounts behind the store's back.
    ///
    /// Returns:
    ///     The count, always `Some`.
    fn revision(&self) -> Option<u64> {
        Some(self.revision.load(Ordering::Acquire))
    }
}
