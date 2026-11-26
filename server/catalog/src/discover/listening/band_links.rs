//! An artist's band links: its MusicBrainz relationships to its members,
//! the bands a person is in, founders, subgroups, collaborations and other
//! names, read with `inc=artist-rels` from the install's MusicBrainz source
//! and kept as a fact ([`KeptFacts`]) for everyone.
//!
//! Only the refresh job reads them, in the background: no reader a person
//! waits on makes this read. A found list is kept for 90 days, an artist
//! with none for 30, and a failed read keeps the rows it had and backs off.
//! The lookup is identity-critical on purpose: a best-effort one turns a
//! MusicBrainz that is down into "no such artist", which would be kept as
//! "no links".
//!
//! Verified against BrainzMash (`https://api.brainzmash.cc/ws/2`) on
//! 2026-11-26, `artist/{id}?inc=artist-rels`, recorded in
//! `server/tests/fixtures/musicbrainz/artist_rels_*.json`: each relation
//! carries `type`, `type-id`, `direction` (`forward` or `backward`),
//! `ended` (a bool), `target-type` `artist` and `artist` with `id` and
//! `name`. Direction is read from the artist looked up: `forward` means it
//! is the one in the band, the founder, the collaborator or the person
//! behind the other name. A member who left and came back is two relations
//! of one kind and direction.

use droppedneedle_library::identify::artist_links::placeholder;
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use droppedneedle_providers::musicbrainz::models::Relation;

use crate::discover::facts::{FactKind, FactSource, KeptFacts, WantOrder};
use crate::discover::ports::BoxFuture;

/// An artist's band links, from the MusicBrainz source.
const LINKS: (FactKind, FactSource) = (FactKind::BandLinks, FactSource::MusicBrainz);

/// What a band link is, by MusicBrainz relationship type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BandLinkKind {
    /// "member of band": a person (or group) in a group.
    MemberOfBand,
    /// "subgroup": a group within another.
    Subgroup,
    /// "founder": the one who founded a group.
    Founder,
    /// "is person": a performance name and the person behind it.
    IsPerson,
    /// "collaboration": an artist in a short-lived project.
    Collaboration,
}

droppedneedle_kernel::spelled!(BandLinkKind {
    MemberOfBand => "member_of_band",
    Subgroup => "subgroup",
    Founder => "founder",
    IsPerson => "is_person",
    Collaboration => "collaboration",
});

impl BandLinkKind {
    /// The kind of the relationship type `type_id`; `None` for every type
    /// a band link is not. The ids are those the recorded answers carry.
    fn of_type(type_id: &str) -> Option<Self> {
        match type_id {
            // member of band
            "5be4c609-9afa-4ea0-910b-12ffb71e3821" => Some(Self::MemberOfBand),
            // subgroup
            "7802f96b-d995-4ce9-8f70-6366faad758e" => Some(Self::Subgroup),
            // founder
            "6ed4bfc4-0a0d-44c0-b025-b7fc4d900b67" => Some(Self::Founder),
            // is person
            "dd9886f2-1dfe-4270-97db-283f6839a666" => Some(Self::IsPerson),
            // collaboration
            "75c09861-6857-4ec0-9729-84eefde7fc86" => Some(Self::Collaboration),
            _ => None,
        }
    }
}

/// Which way a band link runs from the artist that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkDirection {
    /// The artist is the one in the band, the founder, the collaborator or
    /// the person behind the other name.
    Forward,
    /// The linked artist is.
    Backward,
}

droppedneedle_kernel::spelled!(LinkDirection {
    Forward => "forward",
    Backward => "backward",
});

/// One kept band link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BandLink {
    /// The linked artist's MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name.
    pub name: String,
    /// What the link is.
    pub kind: BandLinkKind,
    /// Which way it runs.
    pub direction: LinkDirection,
    /// Whether it has ended (a member who left).
    pub ended: bool,
}

/// The band links among `artist`'s `relations`, in answer order: the five
/// kinds to another artist with a real id and a name, one a kind,
/// direction and artist, ended only when every one of them ended.
pub fn links(artist: &str, relations: &[Relation]) -> Vec<BandLink> {
    let artist = artist.trim().to_ascii_lowercase();
    let mut found: Vec<BandLink> = Vec::new();
    for relation in relations {
        let Some(kind) = BandLinkKind::of_type(&relation.type_id) else {
            continue;
        };
        let direction = match relation.direction.as_deref() {
            Some("forward") => LinkDirection::Forward,
            Some("backward") => LinkDirection::Backward,
            _ => continue,
        };
        let Some(linked) = &relation.artist else {
            continue;
        };
        let mbid = linked.id.trim().to_ascii_lowercase();
        let name = linked.name.trim();
        if !is_valid_mbid(&mbid) || placeholder(&mbid) || mbid == artist || name.is_empty() {
            continue;
        }
        match found
            .iter_mut()
            .find(|kept| kept.mbid == mbid && kept.kind == kind && kept.direction == direction)
        {
            Some(kept) => kept.ended &= relation.ended,
            None => found.push(BandLink {
                mbid,
                name: name.to_owned(),
                kind,
                direction,
                ended: relation.ended,
            }),
        }
    }
    found
}

/// The one outside read behind band links.
pub(crate) trait BandLinkReads: Send + Sync + 'static {
    /// How a read fails.
    type Error: std::fmt::Display + Send + Sync + 'static;
    /// The artist's band links, `None` when MusicBrainz has no such artist.
    fn band_links<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<Vec<BandLink>>, Self::Error>>;
}

/// Read `artist`'s band links and keep them: found ones replace the rows
/// kept, none (or no such artist) is kept as nothing there.
///
/// # Errors
///
/// The read's failure; the links kept before stay and the next try backs
/// off.
pub(crate) async fn fetch<R: BandLinkReads + ?Sized>(
    facts: &KeptFacts,
    reads: &R,
    artist: &str,
    priority: RequestPriority,
) -> Result<(), R::Error> {
    match reads.band_links(artist, priority).await {
        Ok(found) => {
            let kept = facts
                .keep_band_links(artist, found.unwrap_or_default())
                .await;
            if let Err(cause) = kept {
                tracing::warn!(%cause, "band links were read but not kept");
            }
            Ok(())
        }
        Err(error) => {
            let artists = [artist.to_owned()];
            if let Err(cause) = facts.keep_failed(LINKS, &artists, WantOrder::Pool).await {
                tracing::warn!(%cause, "a failed band links read went unrecorded");
            }
            Err(error)
        }
    }
}
