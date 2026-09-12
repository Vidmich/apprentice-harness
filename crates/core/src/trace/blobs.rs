//! Content-addressed blob files: `<root>/<aa>/<sha256hex>`, immutable,
//! written via a temp file and rename. Metadata (size, media type,
//! refcount) lives in the `blobs` table; this module only knows files.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::error::TraceError;

/// Lower-case hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for b in &digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[derive(Debug, Clone)]
pub struct BlobFiles {
    root: PathBuf,
}

impl BlobFiles {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/<aa>/<id>`.
    pub fn path(&self, id: &str) -> PathBuf {
        let shard = id.get(..2).unwrap_or("__");
        self.root.join(shard).join(id)
    }

    /// Writes `data` unless a file with that hash already exists. Returns
    /// `(id, newly_written)`.
    pub fn write(&self, data: &[u8]) -> Result<(String, bool), TraceError> {
        let id = sha256_hex(data);
        let path = self.path(&id);
        if path.is_file() {
            return Ok((id, false));
        }
        let dir = path.parent().expect("blob path has a shard dir");
        fs::create_dir_all(dir).map_err(|e| TraceError::io("creating blob dir", dir, e))?;
        let tmp = dir.join(format!(".{id}.{}.tmp", uuid::Uuid::now_v7().simple()));
        // No fsync: the database itself runs `synchronous=NORMAL`, and a
        // torn file after a crash is caught by `integrity_check`.
        let written = fs::File::create(&tmp).and_then(|mut f| f.write_all(data));
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(TraceError::io("writing blob", &tmp, e));
        }
        match fs::rename(&tmp, &path) {
            Ok(()) => Ok((id, true)),
            // Lost a race against another writer of the same content.
            Err(_) if path.is_file() => {
                let _ = fs::remove_file(&tmp);
                Ok((id, false))
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(TraceError::io("renaming blob", &path, e))
            }
        }
    }

    pub fn read(&self, id: &str) -> Result<Vec<u8>, TraceError> {
        let path = self.path(id);
        match fs::read(&path) {
            Ok(v) => Ok(v),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(TraceError::not_found("blob", id))
            }
            Err(e) => Err(TraceError::io("reading blob", path, e)),
        }
    }

    /// Reads and re-hashes the file.
    pub fn verify(&self, id: &str) -> Result<(), TraceError> {
        let data = self.read(id)?;
        let actual = sha256_hex(&data);
        if actual == id {
            Ok(())
        } else {
            Err(TraceError::BlobCorrupted {
                id: id.to_owned(),
                actual,
            })
        }
    }

    /// Removes the file (used by retention pruning; the table row stays).
    pub fn remove(&self, id: &str) -> Result<(), TraceError> {
        let path = self.path(id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(TraceError::io("removing blob", path, e)),
        }
    }

    /// Total bytes and file count under the root (temp files excluded).
    pub fn disk_usage(&self) -> Result<(u64, u64), TraceError> {
        let mut bytes = 0;
        let mut files = 0;
        let shards = match fs::read_dir(&self.root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
            Err(e) => return Err(TraceError::io("listing blobs", &self.root, e)),
        };
        for shard in shards {
            let shard = shard.map_err(|e| TraceError::io("listing blobs", &self.root, e))?;
            if !shard.path().is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path())
                .map_err(|e| TraceError::io("listing blobs", shard.path(), e))?
            {
                let entry = entry.map_err(|e| TraceError::io("listing blobs", shard.path(), e))?;
                if entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let meta = entry
                    .metadata()
                    .map_err(|e| TraceError::io("stat blob", entry.path(), e))?;
                if meta.is_file() {
                    bytes += meta.len();
                    files += 1;
                }
            }
        }
        Ok((bytes, files))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn write_dedups_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobFiles::new(dir.path().join("blobs"));
        let (id, fresh) = blobs.write(b"hello").unwrap();
        assert!(fresh);
        let (id2, fresh2) = blobs.write(b"hello").unwrap();
        assert_eq!(id, id2);
        assert!(!fresh2);
        assert_eq!(
            blobs.path(&id).parent().unwrap().file_name().unwrap(),
            &id[..2]
        );
        assert_eq!(blobs.read(&id).unwrap(), b"hello");
        blobs.verify(&id).unwrap();

        fs::write(blobs.path(&id), b"tampered").unwrap();
        assert!(matches!(
            blobs.verify(&id),
            Err(TraceError::BlobCorrupted { .. })
        ));
        blobs.remove(&id).unwrap();
        assert!(matches!(blobs.read(&id), Err(TraceError::NotFound { .. })));
        assert_eq!(blobs.disk_usage().unwrap(), (0, 0));
    }
}
