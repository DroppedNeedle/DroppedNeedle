//! Carried sections: one v2 table (or one set of v2 files) each, covering
//! user data, the library catalog and the curator's decisions.
//!
//! The exporter copies every section into the export bundle, a SQLite file
//! beside the export JSON; the importer applies the bundle to v3 one
//! section at a time. Both sides read the same specs here, so the wire
//! shape cannot drift: the bundle holds each row in its v3 shape (v3
//! column names, v2 renames resolved at export), and a column the v2
//! table lacks is left out so v3's default fills it (the importer refuses
//! when v3 has no default for it).
//!
//! Each area keeps its sections in its own module. [`ALL`] lists them in
//! apply order, parents before children. Secrets never go in the bundle:
//! per-user connections travel sealed in the export JSON instead.

pub mod avatars;
pub mod collections;
pub mod compat;
pub mod contributions;
pub mod downloads;
pub mod held;
pub mod history;
pub mod library;
pub mod management;
pub mod prefs;
pub mod releases;
pub mod requests;
pub mod youtube;

/// The kind of v2 library row a carried row points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// A `local_tracks` id.
    Track,
    /// A `local_albums` id.
    Album,
    /// A `local_artists` id.
    Artist,
    /// A `library_reference_tombstones` id.
    Tombstone,
    /// Named per row by another column holding `track`, `album` or
    /// `artist` (favorites).
    ByColumn(&'static str),
}

/// How a carried row holds a v2 library reference until the library
/// carry resolves it. Every unresolved reference is also recorded in
/// `import_pending_links` with this mode, so none is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMode {
    /// The v3 column has no foreign key: the v2 id is written as is.
    AsWritten,
    /// The v3 column has a foreign key: it stays empty until the v3
    /// catalog holds the id, and is filled at once when it already does.
    UntilResolved,
    /// v3 has no column for it: the reference lives only in the
    /// pending-link table.
    LinkOnly,
}

impl LinkMode {
    /// The `import_pending_links.mode` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AsWritten => "as_written",
            Self::UntilResolved => "until_resolved",
            Self::LinkOnly => "link_only",
        }
    }
}

/// One v2 library reference on a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// What the id points at.
    pub kind: LinkKind,
    /// How the v3 row holds it.
    pub mode: LinkMode,
    /// SQL predicate over the bundle row (alias `b`) limiting which rows
    /// hold a library reference here; empty for every row.
    pub when: &'static str,
}

/// Where a column's value comes from in v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2Value {
    /// A v2 column. When the v2 table lacks it, the column stays out of
    /// the bundle.
    Column(&'static str),
    /// An SQL expression over the v2 row (alias `s`, other v2 tables as
    /// `v2.<table>`).
    Expr(&'static str),
}

/// One carried column: its v3 (and bundle) name, where the value comes
/// from in v2, and an optional library reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    /// v3 column name, also the bundle column name.
    pub name: &'static str,
    /// Where the value comes from.
    pub v2: V2Value,
    /// Set when the value is a v2 library id.
    pub link: Option<Link>,
}

impl Column {
    /// A column with the same name on both sides.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            v2: V2Value::Column(name),
            link: None,
        }
    }

    /// The value comes from a differently named v2 column.
    #[must_use]
    pub const fn from_v2(mut self, v2_name: &'static str) -> Self {
        self.v2 = V2Value::Column(v2_name);
        self
    }

    /// The value is computed from the v2 row.
    #[must_use]
    pub const fn from_sql(mut self, expr: &'static str) -> Self {
        self.v2 = V2Value::Expr(expr);
        self
    }

    /// The value is a v2 library id.
    #[must_use]
    pub const fn link(mut self, kind: LinkKind, mode: LinkMode) -> Self {
        self.link = Some(Link {
            kind,
            mode,
            when: "",
        });
        self
    }

    /// The value is a v2 library id on rows matching `when` only.
    #[must_use]
    pub const fn link_when(mut self, kind: LinkKind, mode: LinkMode, when: &'static str) -> Self {
        self.link = Some(Link { kind, mode, when });
        self
    }

    /// True when the v3 table has this column.
    #[must_use]
    pub fn in_target(&self) -> bool {
        !matches!(
            self.link,
            Some(Link {
                mode: LinkMode::LinkOnly,
                ..
            })
        )
    }
}

/// Shorthand for [`Column::new`] in the section tables.
#[must_use]
pub const fn col(name: &'static str) -> Column {
    Column::new(name)
}

/// File sets the exporter reads from the v2 cache dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSet {
    /// `covers/playlists/<playlist id>.<ext>` of carried playlists.
    PlaylistCovers,
    /// `avatars/<user id>.<ext>` of carried users.
    Avatars,
    /// `library-management/blobs/objects/<aa>/<bb>/<sha256>.blob` of the
    /// blob ledger rows already in the bundle, checked against their hash.
    ManagementBlobs,
    /// `held/<name>` named by each carried held-import row, read into the
    /// row's `file` column.
    HeldImports,
}

/// Where a section's rows come from in v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Rows of one v2 table. `filter` is an extra SQL predicate over the
    /// v2 row (alias `s`; other v2 tables are named `v2.<table>`), or
    /// empty for every row. `requires` names other v2 tables the filter
    /// or a column reads; without them the section is empty.
    Table {
        /// v2 table name.
        table: &'static str,
        /// Extra row predicate, empty for none.
        filter: &'static str,
        /// Other v2 tables the section reads.
        requires: &'static [&'static str],
    },
    /// Files in the v2 cache dir.
    Files(FileSet),
    /// Rows of one v2 table (as [`Source::Table`], every row of a live
    /// user that passes `filter`), plus the file each row names.
    TableWithFiles {
        /// v2 table name.
        table: &'static str,
        /// Extra row predicate, empty for none.
        filter: &'static str,
        /// The files the rows name.
        files: FileSet,
    },
}

/// Where a section's rows land in v3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Rows of one v3 table.
    Table(&'static str),
    /// Avatar image files under the v3 cache dir.
    AvatarFiles,
    /// `held_imports` rows whose files land under the v3 cache dir's
    /// `held` folder first.
    HeldFiles,
}

/// A v3 row a carried row hangs off (a foreign key). A carried row whose
/// parent did not land in v3 is left out and counted, never forced in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parent {
    /// v3 parent table.
    pub table: &'static str,
    /// (column on the carried row, column on the parent row) pairs.
    pub columns: &'static [(&'static str, &'static str)],
    /// A row whose reference is empty (a NULL in any of `columns`) needs
    /// no parent: the reference is optional.
    pub optional: bool,
}

impl Parent {
    /// A parent every carried row must have.
    #[must_use]
    pub const fn required(
        table: &'static str,
        columns: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            table,
            columns,
            optional: false,
        }
    }

    /// A parent only rows that hold the reference need.
    #[must_use]
    pub const fn optional(
        table: &'static str,
        columns: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            table,
            columns,
            optional: true,
        }
    }
}

/// One carried section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableSection {
    /// Bundle table name and report entity.
    pub name: &'static str,
    /// v2 origin.
    pub source: Source,
    /// v3 destination.
    pub target: Target,
    /// Carried columns, in v3 naming.
    pub columns: &'static [Column],
    /// Columns naming one row in v3; the conflict identity.
    pub key: &'static [&'static str],
    /// Other v3 uniqueness rules on the target, at most one per section. A
    /// row that would break it counts as a conflict (the existing or first
    /// landing row is kept). Columns compare with `=`, so a row with a NULL
    /// in one never clashes, as in SQLite. The exporter indexes the rule's
    /// columns in the bundle.
    pub unique: &'static [&'static [&'static str]],
    /// Rows this section's rows hang off in v3.
    pub parents: &'static [Parent],
    /// Column holding the owning user id, when rows belong to a user.
    /// Rows of users the export does not carry stay behind.
    pub user_column: Option<&'static str>,
    /// Why v2 rows the filters skip stay behind, for the left-behind list.
    /// Empty when the section adds to rows another section counts.
    pub left_behind: &'static str,
}

impl TableSection {
    /// Spec for one bundle column by name.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|column| column.name == name)
    }
}

/// Every carried bundle section in apply order: parents before children.
/// User data comes first and the library after it, so an import that
/// stops between the two keeps the user data; the importer's link step
/// then points the user data's library references at the carried catalog.
pub const ALL: &[&TableSection] = &[
    &collections::PLAYLISTS,
    &collections::PLAYLIST_TRACKS,
    &collections::PLAYLIST_COVERS,
    &collections::FAVORITES,
    &collections::FAVORITE_NAMES,
    &history::PLAY_HISTORY,
    &requests::REQUESTS,
    &requests::REQUEST_REQUESTERS,
    &requests::REQUEST_DISMISSALS,
    &requests::WANTED_WATCHES,
    &requests::WANTED_SEEN_CANDIDATES,
    &requests::QUOTAS,
    &downloads::DOWNLOAD_TASKS,
    &downloads::DOWNLOAD_ATTEMPTS,
    &downloads::QUARANTINE,
    &prefs::LISTENING_PREFS,
    &prefs::PERSONAL_MIX_APPROVALS,
    &prefs::SECTION_PREFS,
    &prefs::NAVIDROME_FOLDER_PREFS,
    &prefs::NEW_RELEASE_SEEN,
    &compat::PLAY_QUEUES,
    &compat::PLAY_QUEUE_ITEMS,
    &compat::BOOKMARKS,
    &releases::KNOWN_RELEASES,
    &releases::NEW_RELEASE_FEED,
    &youtube::ALBUM_LINKS,
    &youtube::TRACK_LINKS,
    &avatars::AVATARS,
    &library::ARTISTS,
    &library::ALBUMS,
    &library::ALBUM_ARTISTS,
    &library::TRACKS,
    &library::TRACK_ARTISTS,
    &library::TRACK_GENRES,
    &library::TOMBSTONES,
    &library::SOURCE_LINKS,
    &library::ARTIST_ALIASES,
    &library::ALBUM_ALIASES,
    &library::ARTIST_IDENTITIES,
    &library::ALBUM_IDENTITIES,
    &library::TRACK_IDENTITIES,
    &library::REVIEW_DECISIONS,
    &library::KEPT_TAGGED,
    &library::REQUEUED_REVIEWS,
    &library::ALBUM_PINS,
    &library::IDENTIFY_PINS,
    &library::MERGE_CANDIDATES,
    &library::ARTIST_DISMISSALS,
    &management::BLOBS,
    &management::BLOB_BYTES,
    &management::BASELINES,
    &management::TRACK_STATE,
    &management::EDITION_MANIFESTS,
    &management::EDITION_TRACKS,
    &management::EDITION_ACTIVE,
    &management::EXCLUSIONS,
    &management::OVERRIDES,
    &contributions::DRAFTS,
    &contributions::VERIFICATION_JOBS,
    &contributions::CALLBACK_TOKENS,
    &held::HELD_IMPORTS,
];

/// Predicate keeping only rows of users that still exist in v2.
pub(crate) const LIVE_USER: &str = "IN (SELECT id FROM v2.auth_users)";

#[cfg(test)]
mod tests {
    use super::*;

    /// Every section names its key, unique, parent and user columns among
    /// its own target columns, and names are unique: the SQL builders rely
    /// on all of it.
    #[test]
    fn section_specs_are_consistent() {
        let mut names = std::collections::HashSet::new();
        for section in ALL {
            assert!(names.insert(section.name), "duplicate {}", section.name);
            let lands = |name: &str| section.column(name).is_some_and(Column::in_target);
            for key in section.key {
                assert!(lands(key), "{}: key {key}", section.name);
            }
            // The importer's clash test relies on one rule at most.
            assert!(section.unique.len() <= 1, "{}", section.name);
            for rule in section.unique {
                assert!(rule.iter().all(|name| lands(name)), "{}", section.name);
            }
            for parent in section.parents {
                assert!(
                    parent.columns.iter().all(|(name, _)| lands(name)),
                    "{}",
                    section.name
                );
            }
            if let Some(user) = section.user_column {
                assert!(section.column(user).is_some(), "{}", section.name);
            }
        }
    }
}
