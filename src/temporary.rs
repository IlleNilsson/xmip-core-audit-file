//! A test's own folder its audit is written in: a
//! [`file::temporary::Folder`], removed only once audit's keeper kept every
//! record handed to it before (ADR-0065, amendment 2026-11-20). Compiled
//! for tests only, with `test-support`.
//!
//! A record made where its caller must not wait for a disk — an Event
//! subscription's deliveries and its close, a sync link's — is handed to
//! the keeper (`audit::keeper::later`) and written after its caller went on;
//! a program's next direct record, its stop, is kept after it. A test has
//! no stop to record, so a record its keeper kept after the test dropped
//! its folder made the folder and its audit file again, and the folder
//! outlived its test. Dropping this one first waits for the keeper
//! (`audit::keeper::settle`), then removes the folder. A test makes it
//! first, so that it is dropped after every subscription, link and node
//! that audits in it.

use std::ops::Deref;
use std::path::{Path, PathBuf};

use audit::keeper;

/// A test's own folder its audit is written in, removed once audit's keeper
/// kept everything handed to it before.
#[derive(Debug)]
pub struct Folder {
    folder: file::temporary::Folder,
}

impl Folder {
    /// Makes `xmip-<label>-<UUIDv7>` under the system's temporary directory,
    /// as [`file::temporary::Folder::new`] does.
    ///
    /// # Panics
    /// The folder cannot be made.
    #[must_use]
    pub fn new(label: &str) -> Self {
        Self {
            folder: file::temporary::Folder::new(label),
        }
    }
}

impl Deref for Folder {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.folder
    }
}

impl AsRef<Path> for Folder {
    fn as_ref(&self) -> &Path {
        &self.folder
    }
}

/// Where it is, for what takes a path of its own: the folder stays the
/// test's, and is removed when the test drops it.
impl From<&Folder> for PathBuf {
    fn from(folder: &Folder) -> Self {
        PathBuf::from(&folder.folder)
    }
}

impl Drop for Folder {
    /// Waits until the keeper kept every record handed to it before; the
    /// folder is removed after, as its own drop.
    fn drop(&mut self) {
        keeper::settle();
    }
}
