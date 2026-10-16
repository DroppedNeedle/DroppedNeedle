//! Profile pictures. v2 and v3 both keep them as `avatars/{user_id}.{ext}`
//! under the cache dir, so the bytes travel in the bundle and land under
//! the same file name.

use super::{FileSet, Source, TableSection, Target, col};

/// Avatar images of carried users.
pub const AVATARS: TableSection = TableSection {
    name: "avatar",
    source: Source::Files(FileSet::Avatars),
    target: Target::AvatarFiles,
    columns: &[col("user_id"), col("ext"), col("image")],
    key: &["user_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "",
};
