//! Avatar image bytes on disk.

use std::path::{Path, PathBuf};

use super::users::stores::{AvatarStore, BoxFuture, LoadedAvatar, StoreError};

/// Log-safe store failure for avatar IO.
fn internal(error: impl std::fmt::Display) -> StoreError {
    StoreError::Internal(error.to_string())
}

/// Avatar bytes under `<dir>/avatars/{user_id}.{ext}`.
///
/// Only one extension variant exists per user: saving replaces any prior
/// avatar regardless of type. Reads resolve the stored variant by extension.
#[derive(Clone, Debug)]
pub struct FileAvatarStore {
    dir: Option<PathBuf>,
}

impl FileAvatarStore {
    /// Store rooted at `dir`; the `avatars` child is created lazily on save.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: Some(dir.to_owned()),
        }
    }

    /// Test store with no directory: saves fail, loads read as absent.
    #[cfg(any(test, feature = "test-support"))]
    pub fn unwired() -> Self {
        Self { dir: None }
    }

    fn path_for(&self, user_id: &str, ext: &str) -> Option<PathBuf> {
        self.dir
            .as_ref()
            .map(|dir| dir.join("avatars").join(format!("{user_id}.{ext}")))
    }
}

/// Extension plus content type for one supported avatar upload.
fn avatar_variant(content_type: &str) -> Option<(&'static str, &'static str)> {
    match content_type {
        "image/jpeg" => Some(("jpg", "image/jpeg")),
        "image/png" => Some(("png", "image/png")),
        "image/webp" => Some(("webp", "image/webp")),
        "image/gif" => Some(("gif", "image/gif")),
        _ => None,
    }
}

impl AvatarStore for FileAvatarStore {
    fn save<'a>(
        &'a self,
        user_id: &'a str,
        content_type: &'a str,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<String, StoreError>> {
        Box::pin(async move {
            let Some((ext, _)) = avatar_variant(content_type) else {
                return Err(internal("unsupported avatar type"));
            };
            let Some(target) = self.path_for(user_id, ext) else {
                return Err(internal("auth avatar store is not wired"));
            };
            // Blocking file work leaves the IO loop.
            let owned: Vec<u8> = bytes.to_vec();
            let user_id = user_id.to_owned();
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                let parent = target
                    .parent()
                    .ok_or_else(|| "avatar dir has no parent".to_owned())?;
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                for old in ["jpg", "png", "webp", "gif"] {
                    if old == ext {
                        continue;
                    }
                    let stale = parent.join(format!("{user_id}.{old}"));
                    if stale != target {
                        let _ = std::fs::remove_file(stale);
                    }
                }
                let tmp = parent.join(format!(".{user_id}.{ext}.tmp"));
                std::fs::write(&tmp, &owned).map_err(|error| error.to_string())?;
                std::fs::rename(&tmp, &target).map_err(|error| error.to_string())?;
                Ok(())
            })
            .await
            .map_err(|error| internal(format!("avatar save panicked: {error}")))?
            .map_err(internal)?;
            Ok(ext.to_owned())
        })
    }

    fn load<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LoadedAvatar>, StoreError>> {
        Box::pin(async move {
            let Some(dir) = self.dir.clone() else {
                return Ok(None);
            };
            let user_id = user_id.to_owned();
            tokio::task::spawn_blocking(move || {
                for (ext, content_type) in [
                    ("jpg", "image/jpeg"),
                    ("png", "image/png"),
                    ("webp", "image/webp"),
                    ("gif", "image/gif"),
                ] {
                    let path = dir.join("avatars").join(format!("{user_id}.{ext}"));
                    if let Ok(bytes) = std::fs::read(&path) {
                        return Ok(Some((bytes, content_type.to_owned())));
                    }
                }
                Ok(None)
            })
            .await
            .map_err(|error| internal(format!("avatar load panicked: {error}")))?
        })
    }
}
