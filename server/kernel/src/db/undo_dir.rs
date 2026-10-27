//! The folder of undo copies beside the database.
//!
//! The bytes Undo and "Restore originals" need (tag snapshots, kept
//! pictures and sidecars, v2 baselines) live here rather than in the
//! database, each file named by the SHA-256 of its bytes:
//! `undo/<first two hex digits>/<64 hex digits>`. A file is written to a
//! temporary name, synced, renamed into place and its folder synced, so a
//! name never holds part of a file. Reads hash the bytes and refuse a file
//! that does not match its name.
//!
//! The database still says who needs each file. This module is only the
//! layout, the verified read, the atomic put, removal and the folder's
//! lock; which files are still needed is the library's business.
//!
//! The lock is an OS file lock on `undo/.lock`. A put holds it shared
//! until the row that refers to the file has committed, a backup holds it
//! shared while it copies, and the sweep takes it exclusively before it
//! removes anything, so a sweep never removes a file between its put and
//! its reference, or under a backup. Each hold opens its own descriptor:
//! the lock belongs to the open file, so two holds in one process
//! exclude each other as two processes would. Like the database, the
//! folder must be on a local filesystem for the lock to hold.

use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};

/// The folder's name beside the database file.
pub const UNDO_DIR_NAME: &str = "undo";
/// The lock file inside the folder.
const LOCK_FILE: &str = ".lock";
/// Prefix of a file being written.
const TEMP_PREFIX: &str = ".tmp-";

/// The undo folder that belongs to the database at `db_path`.
pub fn beside(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(UNDO_DIR_NAME)
}

/// SHA-256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    text
}

/// True for a name this folder can hold: 64 lowercase hex digits.
pub fn is_blob_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The 256 shard folder names, `00` to `ff`.
pub fn shard_names() -> impl Iterator<Item = String> {
    (0u16..256).map(|n| format!("{n:02x}"))
}

/// What a read found under one name.
#[derive(Debug, PartialEq, Eq)]
pub enum Stored {
    /// The bytes, checked against the name.
    Found(Vec<u8>),
    /// No file by that name.
    Missing,
    /// A file whose bytes do not match its name. Never used.
    Corrupt,
}

/// What one shard folder holds.
#[derive(Debug, Default)]
pub struct ShardEntries {
    /// Blob names.
    pub blobs: Vec<String>,
    /// Files left mid-write.
    pub temps: Vec<PathBuf>,
}

/// A held lock on the folder; released on drop.
#[derive(Debug)]
pub struct Hold {
    _file: File,
}

/// One undo folder (the live one, a backup's pool, a restore's staging).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoDir {
    root: PathBuf,
}

impl UndoDir {
    /// The folder at `root`. Nothing is created until the first hold.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The folder's path.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Wait for the shared lock: puts and backups.
    pub fn hold_shared(&self) -> io::Result<Hold> {
        let file = self.lock_file()?;
        file.lock_shared()?;
        Ok(Hold { _file: file })
    }

    /// Wait for the exclusive lock.
    pub fn hold_exclusive(&self) -> io::Result<Hold> {
        let file = self.lock_file()?;
        file.lock()?;
        Ok(Hold { _file: file })
    }

    /// The exclusive lock if nobody holds the folder now, else `None`.
    pub fn try_hold_exclusive(&self) -> io::Result<Option<Hold>> {
        let file = self.lock_file()?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Hold { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    fn lock_file(&self) -> io::Result<File> {
        ensure_dir(&self.root)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(LOCK_FILE))
    }

    /// Where `sha256` lives, or `None` for a name that is not a hash.
    pub fn file_path(&self, sha256: &str) -> Option<PathBuf> {
        is_blob_name(sha256).then(|| self.root.join(&sha256[..2]).join(sha256))
    }

    /// Store `bytes` under their hash and return it. A file already there
    /// that verifies is kept; one that does not is replaced. When this
    /// returns, the file and its name are on disk.
    pub fn put(&self, hold: &Hold, bytes: &[u8]) -> io::Result<String> {
        let sha256 = sha256_hex(bytes);
        self.put_named(hold, &sha256, bytes)?;
        Ok(sha256)
    }

    /// [`put`](Self::put) for bytes already known to hash to `sha256`.
    fn put_named(&self, _hold: &Hold, sha256: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self.file_path(sha256).ok_or_else(|| bad_name(sha256))?;
        let shard = self.root.join(&sha256[..2]);
        ensure_dir(&self.root)?;
        ensure_dir(&shard)?;
        if let Stored::Found(_) = read_file(&path, sha256)? {
            // Another put may have renamed it in without its folder sync
            // having finished yet.
            return sync_dir(&shard);
        }
        let (temp, mut file) = create_temp(&shard, sha256)?;
        let written = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| std::fs::rename(&temp, &path));
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
        sync_dir(&shard)
    }

    /// Read `sha256`, checked against its name.
    pub fn read(&self, sha256: &str) -> io::Result<Stored> {
        match self.file_path(sha256) {
            Some(path) => read_file(&path, sha256),
            None => Ok(Stored::Missing),
        }
    }

    /// True when a file by that name is there (its bytes are not read).
    pub fn contains(&self, sha256: &str) -> bool {
        self.file_path(sha256).is_some_and(|path| path.is_file())
    }

    /// Size of the file under `sha256`, when there is one.
    pub fn size_of(&self, sha256: &str) -> Option<u64> {
        let path = self.file_path(sha256)?;
        std::fs::metadata(path).ok().map(|meta| meta.len())
    }

    /// Copy `sha256` from this folder into `dest`, checked on the way.
    /// A file already in `dest` is trusted by its name.
    pub fn copy_to(&self, dest: &UndoDir, dest_hold: &Hold, sha256: &str) -> io::Result<Copied> {
        if let Some(size) = dest.size_of(sha256) {
            return Ok(Copied::Already(size));
        }
        match self.read(sha256)? {
            Stored::Found(bytes) => {
                dest.put_named(dest_hold, sha256, &bytes)?;
                Ok(Copied::Copied(bytes.len() as u64))
            }
            Stored::Missing => Ok(Copied::Missing),
            Stored::Corrupt => Ok(Copied::Corrupt),
        }
    }

    /// Remove the file under `sha256` and sync its folder. Callers hold
    /// the folder exclusively. False when it was not there.
    pub fn remove(&self, _hold: &Hold, sha256: &str) -> io::Result<bool> {
        let path = self.file_path(sha256).ok_or_else(|| bad_name(sha256))?;
        match std::fs::remove_file(&path) {
            Ok(()) => {
                sync_dir(&self.root.join(&sha256[..2]))?;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Remove a file left mid-write. Callers hold the folder exclusively,
    /// so no put is writing it.
    pub fn remove_temp(&self, _hold: &Hold, path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// The blobs and leftover temporary files in one shard folder.
    pub fn shard(&self, shard: &str) -> io::Result<ShardEntries> {
        let mut found = ShardEntries::default();
        let entries = match std::fs::read_dir(self.root.join(shard)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(found),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(TEMP_PREFIX) {
                found.temps.push(entry.path());
            } else if is_blob_name(&name) && name.starts_with(shard) {
                found.blobs.push(name);
            }
        }
        Ok(found)
    }
}

/// What a copy found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copied {
    /// Written, this many bytes.
    Copied(u64),
    /// Already there, this many bytes.
    Already(u64),
    /// Not in the source folder.
    Missing,
    /// In the source folder with bytes that do not match its name.
    Corrupt,
}

fn bad_name(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{name:?} is not a content hash"),
    )
}

fn read_file(path: &Path, sha256: &str) -> io::Result<Stored> {
    match std::fs::read(path) {
        Ok(bytes) if sha256_hex(&bytes) == sha256 => Ok(Stored::Found(bytes)),
        Ok(_) => Ok(Stored::Corrupt),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Stored::Missing),
        Err(error) => Err(error),
    }
}

/// A new temporary file beside where `sha256` goes. The process id keeps
/// two processes apart; `create_new` keeps two threads apart.
fn create_temp(shard: &Path, sha256: &str) -> io::Result<(PathBuf, File)> {
    let pid = std::process::id();
    for n in 0..1024u32 {
        let path = shard.join(format!("{TEMP_PREFIX}{sha256}-{pid}-{n}"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free temporary name in the undo folder",
    ))
}

/// Make `dir` if it is not there, syncing its parent when it was made so
/// the new folder's name survives a power cut.
fn ensure_dir(dir: &Path) -> io::Result<()> {
    match std::fs::create_dir(dir) {
        Ok(()) => match dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            Some(parent) => sync_dir(parent),
            None => Ok(()),
        },
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                ensure_dir(parent)?;
            }
            ensure_dir(dir)
        }
        Err(error) => Err(error),
    }
}

/// Sync a folder, so a rename or removal in it is on disk.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}
