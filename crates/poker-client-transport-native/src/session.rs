//! Filesystem-backed session-key persistence.
//!
//! The session key is the only durable client-side artefact
//! required to resume after a crash / restart / device swap (per
//! [`CLIENT_PRINCIPLES.md`](../../../../docs/CLIENT_PRINCIPLES.md) §3).
//! A server-minted UTF-8 string, opaque to us; the host decides
//! where to store it.
//!
//! Writes are atomic via the standard temp-file-plus-rename dance
//! so a crash mid-write can't leave a truncated session key on
//! disk.

use std::io;
use std::path::{Path, PathBuf};

/// Filesystem-backed session-key store.
#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    /// `path` is the file that holds the session key. The parent
    /// directory is created lazily on first `save`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the persisted session key, if any. Returns `Ok(None)`
    /// when the file does not exist or is empty after trimming;
    /// propagates other I/O errors (permission denied, etc.).
    pub fn load(&self) -> io::Result<Option<String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(trimmed.to_string()))
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Atomically replace the stored key. Creates the parent
    /// directory if missing.
    pub fn save(&self, key: &str) -> io::Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, key)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Remove any persisted session key. Silent on "file not
    /// found" so repeat clears are harmless.
    pub fn clear(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.key");
        let s = SessionStore::new(path.clone());

        assert_eq!(s.load().unwrap(), None);
        s.save("abc123").unwrap();
        assert_eq!(s.load().unwrap().as_deref(), Some("abc123"));

        // Overwrite replaces, not appends.
        s.save("def456").unwrap();
        assert_eq!(s.load().unwrap().as_deref(), Some("def456"));

        s.clear().unwrap();
        assert_eq!(s.load().unwrap(), None);

        // clear() is idempotent.
        s.clear().unwrap();
    }

    #[test]
    fn save_creates_missing_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/dir/session.key");
        let s = SessionStore::new(path.clone());
        s.save("xyz").unwrap();
        assert!(path.exists());
        assert_eq!(s.load().unwrap().as_deref(), Some("xyz"));
    }

    #[test]
    fn empty_file_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.key");
        std::fs::write(&path, "   \n").unwrap();
        let s = SessionStore::new(path);
        assert_eq!(s.load().unwrap(), None);
    }
}
