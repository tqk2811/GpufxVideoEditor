//! Keeps two processes from editing one project file: each would overwrite what the other saved.
//!
//! The lock is a file beside the project, held open without sharing and removed when closed. The
//! system closes it for a process that dies too, so a crash leaves nothing that blocks the next run.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// `FILE_FLAG_DELETE_ON_CLOSE`.
const DELETE_ON_CLOSE: u32 = 0x0400_0000;
/// `ERROR_SHARING_VIOLATION`: another process holds the lock.
const SHARING_VIOLATION: i32 = 32;

/// Holds a project file for this process until dropped.
#[derive(Debug)]
pub struct ProjectLock {
    file: PathBuf,
    _handle: File,
}

fn lock_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

impl ProjectLock {
    /// `Ok(None)` when another process holds `file`. A folder where no lock file can be made is
    /// not a reason to refuse the project, so that counts as acquired without a lock.
    pub fn acquire(file: &Path) -> Result<Option<Self>, String> {
        let handle = OpenOptions::new().read(true).write(true).create(true).truncate(false).share_mode(0).custom_flags(DELETE_ON_CLOSE).open(lock_path(file));
        match handle {
            Ok(handle) => Ok(Some(Self { file: file.to_path_buf(), _handle: handle })),
            Err(e) if e.raw_os_error() == Some(SHARING_VIOLATION) => Ok(None),
            Err(e) => Err(format!("creating the lock of {} failed: {e}", file.display())),
        }
    }

    /// The project file this lock holds.
    pub fn file(&self) -> &Path {
        &self.file
    }
}

/// Whether another process holds `file` right now.
pub fn held_elsewhere(file: &Path) -> bool {
    matches!(ProjectLock::acquire(file), Ok(None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpufx-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_held_project_cannot_be_held_again_until_released_and_leaves_no_file() {
        let dir = folder("held");
        let file = dir.join("p.json");
        let first = ProjectLock::acquire(&file).unwrap().expect("free");
        assert_eq!(first.file(), file);
        assert!(ProjectLock::acquire(&file).unwrap().is_none());
        assert!(held_elsewhere(&file));
        drop(first);
        assert!(!held_elsewhere(&file));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "the lock file went with the lock");
    }
}
