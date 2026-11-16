//! Albums the library lists more than once: two copies of one release,
//! copies naming different releases of one group, and a copy with fewer
//! indexed tracks than a sibling. Read-only: nothing here writes, plans a
//! write or asks a provider.

use std::collections::BTreeMap;

use droppedneedle_kernel::db::{CancelFlag, Db};

use crate::identity::DecisionSource;
use crate::jobs::models::OperationError;

pub mod sql;

/// Why a group is a duplicate. A group lists every reason that applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateClass {
    /// Two or more copies name the same release.
    SameRelease,
    /// The copies name more than one release.
    DifferentReleases,
    /// A copy has fewer indexed tracks than another.
    IncompleteCopy,
}

/// How one copy names its group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateEvidence {
    /// Its match row names the group.
    StoredMatch,
    /// Its files' tags name the group.
    FileTags,
}

/// One album in a duplicate group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCopy {
    pub album_id: String,
    pub title: String,
    pub artist: String,
    /// Indexed tracks the album holds.
    pub track_count: u64,
    /// Their bytes.
    pub total_size_bytes: u64,
    /// The distinct file formats of its indexed tracks, sorted.
    pub formats: Vec<String>,
    /// The release its match row names, when there is one.
    pub release_mbid: Option<String>,
    /// Who set that match.
    pub decision_source: Option<DecisionSource>,
    pub evidence: DuplicateEvidence,
    /// Its match is a guess its files contradict.
    pub doubted: bool,
}

/// Albums the library lists more than once, with their copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    /// The release group the copies share.
    pub id: String,
    /// The release two or more copies name, when there is one.
    pub release_mbid: Option<String>,
    pub classes: Vec<DuplicateClass>,
    pub copies: Vec<DuplicateCopy>,
}

/// One page of duplicate groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicatePage {
    pub groups: Vec<DuplicateGroup>,
    /// The group the next page starts after.
    pub next: Option<String>,
}

/// The duplicates list over the library database.
#[derive(Clone)]
pub struct Duplicates {
    db: Db,
}

impl Duplicates {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// One page of groups, starting after the `after` group id.
    pub fn page(
        &self,
        limit: usize,
        after: Option<String>,
    ) -> Result<DuplicatePage, OperationError> {
        let limit = limit.max(1);
        let rows = self
            .db
            .read_long("library.duplicate_albums", &CancelFlag::never(), |conn| {
                let mut statement = conn.prepare_cached(sql::DUPLICATE_ALBUMS.text())?;
                let rows = statement
                    .query_map(
                        rusqlite::params![after.as_deref(), limit as i64 + 1],
                        sql::DuplicateAlbumRow::read,
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, OperationError>(rows)
            })?;
        Ok(fold(rows, limit))
    }
}

/// The page's groups, built from rows ordered by group, then album.
fn fold(rows: Vec<sql::DuplicateAlbumRow>, limit: usize) -> DuplicatePage {
    let mut groups: Vec<DuplicateGroup> = Vec::new();
    for row in rows {
        let copy = DuplicateCopy {
            album_id: row.album_id,
            title: row.title,
            artist: row.artist_name,
            track_count: row.track_count,
            total_size_bytes: row.total_size_bytes,
            formats: formats(row.formats),
            release_mbid: row.release_mbid,
            decision_source: row.decision_source,
            evidence: if row.stored_match {
                DuplicateEvidence::StoredMatch
            } else {
                DuplicateEvidence::FileTags
            },
            doubted: row.doubted,
        };
        match groups.last_mut() {
            Some(group) if group.id == row.group_mbid => group.copies.push(copy),
            _ => groups.push(DuplicateGroup {
                id: row.group_mbid.clone(),
                release_mbid: row.shared_release.clone(),
                classes: Vec::new(),
                copies: vec![copy],
            }),
        }
    }
    for group in &mut groups {
        group.classes = classes(&group.copies);
    }
    let more = groups.len() > limit;
    groups.truncate(limit);
    let next = if more {
        groups.last().map(|group| group.id.clone())
    } else {
        None
    };
    DuplicatePage { groups, next }
}

/// The formats a copy's tracks use, split from the group's list.
fn formats(raw: Option<String>) -> Vec<String> {
    let mut formats: Vec<String> = raw
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|format| !format.is_empty())
        .map(str::to_owned)
        .collect();
    formats.sort();
    formats.dedup();
    formats
}

/// Every reason the copies make the group a duplicate.
fn classes(copies: &[DuplicateCopy]) -> Vec<DuplicateClass> {
    let mut releases: BTreeMap<&str, u32> = BTreeMap::new();
    for copy in copies {
        if let Some(release) = copy.release_mbid.as_deref() {
            *releases.entry(release).or_default() += 1;
        }
    }
    let mut classes = Vec::new();
    if releases.values().any(|copies| *copies > 1) {
        classes.push(DuplicateClass::SameRelease);
    }
    if releases.len() > 1 {
        classes.push(DuplicateClass::DifferentReleases);
    }
    let (fewest, most) = copies.iter().fold((u64::MAX, 0), |(fewest, most), copy| {
        (fewest.min(copy.track_count), most.max(copy.track_count))
    });
    if fewest < most {
        classes.push(DuplicateClass::IncompleteCopy);
    }
    classes
}
