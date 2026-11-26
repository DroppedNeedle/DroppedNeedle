//! Band links: albums by the artists the person's seeds are linked to
//! through their bands, read from kept rows only.
//!
//! - The [`proposed::BAND_SEEDS`] heaviest seeds have their kept band links
//!   read. Each link gives the linked artist a role beside the seed
//!   ([`role`]): a person in the seed band ([`BandLinkRole::Member`]), a
//!   band the seed is in ([`BandLinkRole::Band`]), the seed under another
//!   name ([`BandLinkRole::OtherName`]), or a subgroup
//!   ([`BandLinkRole::SharedMember`]).
//! - A seed band with at most [`proposed::BAND_HUB`] members has up to
//!   [`proposed::MEMBERS_PER_BAND`] of them (current ones first) read in
//!   turn, [`proposed::MEMBER_READS`] at most a build; each member's other
//!   bands share a member with the seed ([`BandLinkRole::SharedMember`],
//!   through that member). A member in more than
//!   [`proposed::MEMBER_HUB`] bands gives none.
//! - A linked artist must be new to the person: not known, not the
//!   library's, not turned down, not resting. The
//!   [`proposed::LINKED_ARTISTS`] strongest (the seed's weight times the
//!   role's strength, then by id) have their kept album lists read and give
//!   up to [`proposed::ALBUMS_PER_ARTIST`] albums each through the album
//!   gate and the pool's gates, as `thread` candidates with one `fact` path.
//! - Links or an album list not kept yet are wanted for the kept facts'
//!   refresh, and the build waits for them; nothing is asked of a provider
//!   here.

use std::cmp::Ordering;
use std::collections::HashMap;

use droppedneedle_kernel::db::DbError;
use droppedneedle_library::identify::artist_links::placeholder;
use droppedneedle_providers::musicbrainz::is_valid_mbid;

use super::super::candidate::{
    BandLinkRole, Candidate, CandidateKind, FactLink, Path, PathArtist, PathKind, PathSource, Via,
};
use super::super::rank::{self, AlbumGates, ArtistGates, Seed, proposed};
use super::super::select::Exposure;
use crate::discover::facts::{FactKind, FactSource, Kept, KeptFacts, Looked, Notes, WantOrder};
use crate::discover::listening::band_links::{BandLink, BandLinkKind, LinkDirection};
use crate::discover::listening::{RankedAlbum, gate};

/// What a link makes its artist beside the artist that holds it.
pub(crate) fn role(link: &BandLink) -> BandLinkRole {
    match (link.kind, link.direction) {
        (BandLinkKind::IsPerson, _) => BandLinkRole::OtherName,
        (BandLinkKind::Subgroup, _) => BandLinkRole::SharedMember,
        // Member of band, founder, collaboration: backward from the band
        // or project, forward from the one in it.
        (_, LinkDirection::Backward) => BandLinkRole::Member,
        (_, LinkDirection::Forward) => BandLinkRole::Band,
    }
}

/// One artist a seed's band links lead to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Linked {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name.
    pub name: String,
    /// The seed it was found through.
    pub seed: PathArtist,
    /// That seed's taste group.
    pub group: Option<i64>,
    /// What it is to the seed.
    pub role: BandLinkRole,
    /// The members it runs through, for a band sharing them.
    pub vias: Vec<Via>,
    /// The seed's weight times the role's strength.
    pub strength: f64,
    /// When the links that name it were fetched (the seed's, or the
    /// member's for a band sharing one), unix ms.
    pub fetched_at: Option<i64>,
    /// Whether the link has ended (a member who left).
    pub ended: bool,
    /// Its listeners, when counted.
    pub listeners: Option<i64>,
}

/// What the band links source read.
#[derive(Debug, Clone, Default)]
pub(crate) struct Reach {
    /// Each linked artist kept, strongest first, with its kept album list.
    pub found: Vec<(Linked, Vec<RankedAlbum>)>,
    /// The artists whose links or album list are not kept yet.
    pub missing: Vec<String>,
}

impl Reach {
    /// Every release group the lists name, for the ownership read.
    pub(crate) fn groups(&self) -> impl Iterator<Item = String> + '_ {
        self.found
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .map(|row| row.album.release_group_mbid.to_lowercase())
    }
}

/// `seed` as a path names it.
fn path_artist(seed: &Seed) -> PathArtist {
    PathArtist {
        mbid: seed.mbid.clone(),
        name: seed.name.clone(),
        plays: seed.plays.max(0),
        weight: seed.weight,
        albums_owned: seed.albums_owned.max(0),
    }
}

/// `a` before `b` by listeners: a counted artist first, the most first.
fn louder(a: Option<i64>, b: Option<i64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The kept listeners of `artists` not in `heard` yet, added to it: counted
/// with none is 0, not counted is unknown. One not counted is wanted for
/// the listener batch while ListenBrainz reads are on (`counted`).
async fn hear(
    facts: &KeptFacts,
    (notes, now, counted): (&mut Notes, i64, bool),
    artists: Vec<String>,
    heard: &mut HashMap<String, Option<i64>>,
) -> Result<(), DbError> {
    let asked: Vec<String> = artists
        .into_iter()
        .filter(|artist| !heard.contains_key(artist))
        .collect();
    if asked.is_empty() {
        return Ok(());
    }
    let mut looked = facts.listeners(&asked).await?;
    for artist in asked {
        let seen = looked.remove(&artist).unwrap_or_else(Looked::missing);
        if counted {
            let what = (
                FactKind::Listeners,
                FactSource::ListenBrainz,
                WantOrder::Pool,
            );
            notes.saw(now, what, &artist, &seen);
        }
        let listeners = match seen.kept {
            Kept::Found(found) if counted => Some(found.listeners.unwrap_or(0)),
            Kept::Found(found) => found.listeners,
            Kept::Nothing if counted => Some(0),
            _ => None,
        };
        heard.insert(artist, listeners);
    }
    Ok(())
}

/// `links` one per artist of `role`, in answer order, current first.
fn of_role(links: &[BandLink], wanted: BandLinkRole) -> Vec<&BandLink> {
    let mut found: Vec<&BandLink> = Vec::new();
    for link in links.iter().filter(|link| role(link) == wanted) {
        match found.iter().position(|kept| kept.mbid == link.mbid) {
            // Kept as current while any of its links is.
            Some(at) if found[at].ended && !link.ended => found[at] = link,
            Some(_) => {}
            None => found.push(link),
        }
    }
    found.sort_by_key(|link| link.ended);
    found
}

/// The artists found, by id, the strongest way each was reached.
#[derive(Default)]
struct Found(HashMap<String, Linked>);

impl Found {
    /// Note `linked`: it replaces a weaker way to its artist, and one more
    /// member of the same seed's shared band joins its members.
    fn add(&mut self, linked: Linked) {
        match self.0.get_mut(&linked.mbid) {
            Some(kept)
                if kept.seed.mbid == linked.seed.mbid
                    && kept.role == BandLinkRole::SharedMember
                    && linked.role == BandLinkRole::SharedMember =>
            {
                for via in linked.vias {
                    if !kept.vias.contains(&via) {
                        kept.vias.push(via);
                    }
                }
            }
            Some(kept) if linked.strength > kept.strength => *kept = linked,
            Some(_) => {}
            None => {
                self.0.insert(linked.mbid.clone(), linked);
            }
        }
    }
}

/// Read what `seeds` (heaviest first) are linked to, through what may not
/// be offered (`gates`, `exposure`), with their kept album lists, counted
/// where ListenBrainz reads are on (`counted`). What is not kept is wanted.
///
/// # Errors
///
/// A read's failure.
pub(crate) async fn read(
    facts: &KeptFacts,
    seeds: &[Seed],
    (gates, exposure): (&ArtistGates<'_>, &Exposure),
    counted: bool,
) -> Result<Reach, DbError> {
    let now = facts.now();
    let mut notes = Notes::default();
    let mut reach = Reach::default();
    let links = (
        FactKind::BandLinks,
        FactSource::MusicBrainz,
        WantOrder::Pool,
    );

    let mut chosen: Vec<&Seed> = seeds
        .iter()
        .filter(|seed| is_valid_mbid(&seed.mbid) && !placeholder(&seed.mbid))
        .collect();
    chosen.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| a.mbid.cmp(&b.mbid))
    });
    chosen.truncate(proposed::BAND_SEEDS);
    let ids: Vec<String> = chosen.iter().map(|seed| seed.mbid.clone()).collect();
    let mut looked = facts.links(&ids).await?;
    let mut held: Vec<(&Seed, Vec<BandLink>, Option<i64>)> = Vec::new();
    for seed in chosen {
        let seen = looked.remove(&seed.mbid).unwrap_or_else(Looked::missing);
        notes.saw(now, links, &seed.mbid, &seen);
        match seen.kept {
            Kept::Missing => reach.missing.push(seed.mbid.clone()),
            Kept::Nothing => {}
            Kept::Found(rows) => held.push((seed, rows, seen.fetched_at)),
        }
    }

    // Every direct link's listeners, so a band's best-known members come
    // first.
    let mut heard: HashMap<String, Option<i64>> = HashMap::new();
    let direct: Vec<String> = held
        .iter()
        .flat_map(|(_, rows, _)| rows.iter().map(|link| link.mbid.clone()))
        .collect();
    hear(facts, (&mut notes, now, counted), direct, &mut heard).await?;
    let listeners =
        |heard: &HashMap<String, Option<i64>>, mbid: &str| heard.get(mbid).copied().flatten();

    // The members of each seed band whose other bands are followed: the
    // most listened first, current before ended, then by id.
    let mut asked: Vec<(usize, Via)> = Vec::new();
    for (at, (_, rows, _)) in held.iter().enumerate() {
        let mut members = of_role(rows, BandLinkRole::Member);
        if members.len() > proposed::BAND_HUB {
            continue;
        }
        members.sort_by(|a, b| {
            louder(listeners(&heard, &a.mbid), listeners(&heard, &b.mbid))
                .then_with(|| a.ended.cmp(&b.ended))
                .then_with(|| a.mbid.cmp(&b.mbid))
        });
        for member in members.into_iter().take(proposed::MEMBERS_PER_BAND) {
            if asked.len() >= proposed::MEMBER_READS {
                break;
            }
            asked.push((
                at,
                Via {
                    mbid: member.mbid.clone(),
                    name: member.name.clone(),
                },
            ));
        }
    }
    let member_ids: Vec<String> = asked.iter().map(|(_, via)| via.mbid.clone()).collect();
    let member_links = facts.links(&member_ids).await?;

    let mut found = Found::default();
    for (seed, rows, fetched_at) in &held {
        for link in rows {
            let role = role(link);
            found.add(Linked {
                mbid: link.mbid.clone(),
                name: link.name.clone(),
                seed: path_artist(seed),
                group: seed.group,
                role,
                vias: Vec::new(),
                strength: seed.weight * role.strength(),
                fetched_at: *fetched_at,
                ended: link.ended,
                listeners: None,
            });
        }
    }
    for (at, member) in &asked {
        let (seed, _, _) = &held[*at];
        let seen = member_links
            .get(&member.mbid)
            .cloned()
            .unwrap_or_else(Looked::missing);
        notes.saw(now, links, &member.mbid, &seen);
        let fetched_at = seen.fetched_at;
        let rows = match seen.kept {
            Kept::Missing => {
                reach.missing.push(member.mbid.clone());
                continue;
            }
            Kept::Nothing => continue,
            Kept::Found(rows) => rows,
        };
        let bands = of_role(&rows, BandLinkRole::Band);
        if bands.len() > proposed::MEMBER_HUB {
            continue;
        }
        for band in bands.into_iter().filter(|band| band.mbid != seed.mbid) {
            let role = BandLinkRole::SharedMember;
            found.add(Linked {
                mbid: band.mbid.clone(),
                name: band.name.clone(),
                seed: path_artist(seed),
                group: seed.group,
                role,
                vias: vec![member.clone()],
                strength: seed.weight * role.strength(),
                fetched_at,
                ended: band.ended,
                listeners: None,
            });
        }
    }

    let mut linked: Vec<Linked> = found
        .0
        .into_values()
        .filter(|linked| {
            !gates.knows(&linked.mbid)
                && !gates.refuses(&linked.mbid)
                && !exposure.rests_artist(&linked.mbid)
        })
        .collect();
    // Their listener counts, so the artist floor applies and the
    // best-known come first: one not counted is wanted in the listener batch
    // and passes, as an unknown count does.
    let ids: Vec<String> = linked.iter().map(|artist| artist.mbid.clone()).collect();
    hear(facts, (&mut notes, now, counted), ids, &mut heard).await?;
    for artist in &mut linked {
        artist.listeners = listeners(&heard, &artist.mbid);
    }
    linked.sort_by(|a, b| {
        b.strength
            .total_cmp(&a.strength)
            .then_with(|| louder(a.listeners, b.listeners))
            .then_with(|| a.ended.cmp(&b.ended))
            .then_with(|| a.mbid.cmp(&b.mbid))
    });
    // At most a few from one seed, so one large band cannot take them all.
    let mut per_seed: HashMap<String, usize> = HashMap::new();
    linked.retain(|artist| {
        let taken = per_seed.entry(artist.seed.mbid.clone()).or_default();
        *taken += 1;
        *taken <= proposed::LINKED_PER_SEED
    });
    linked.truncate(proposed::LINKED_ARTISTS);

    for artist in linked {
        let (list, counts) = facts.albums(&artist.mbid).await?;
        notes.saw(
            now,
            (FactKind::Albums, FactSource::MusicBrainz, WantOrder::Pool),
            &artist.mbid,
            &list,
        );
        if counted && matches!(list.kept, Kept::Found(_)) {
            notes.saw(
                now,
                (
                    FactKind::AlbumCounts,
                    FactSource::ListenBrainz,
                    WantOrder::Pool,
                ),
                &artist.mbid,
                &counts,
            );
            // An album list whose counts are not read yet waits for them,
            // as the walk's do.
            if matches!(counts.kept, Kept::Missing) {
                reach.missing.push(artist.mbid.clone());
            }
        }
        let counts_found = counted && matches!(counts.kept, Kept::Found(()));
        match list.kept {
            Kept::Missing => reach.missing.push(artist.mbid.clone()),
            Kept::Nothing => {}
            Kept::Found(mut rows) => {
                // The artist's albums were counted: one the answer left out
                // has no listeners.
                if counts_found {
                    for row in &mut rows {
                        row.album.facts.listeners.get_or_insert(0);
                    }
                }
                reach.found.push((artist, rows));
            }
        }
    }
    if let Err(cause) = facts.note(notes).await {
        tracing::warn!(%cause, "the band links' wants went unnoted");
    }
    Ok(reach)
}

/// The `thread` candidates `reach` gives through `gates`, strongest first.
pub(crate) fn candidates(reach: &Reach, gates: &AlbumGates<'_>) -> Vec<Candidate> {
    let mut found = Vec::new();
    for (linked, rows) in &reach.found {
        let via = match linked.vias.as_slice() {
            [one] => Some(one.clone()),
            _ => None,
        };
        let path = Path {
            kind: PathKind::Fact,
            source: PathSource::Musicbrainz,
            fetched_at: linked.fetched_at,
            artist: Some(linked.seed.clone()),
            link: None,
            known_credit: None,
            fact: Some(FactLink {
                role: linked.role,
                via,
                artist_name: Some(linked.name.clone()),
            }),
        };
        let mut taken = 0;
        let mut seen = Vec::new();
        for row in rank::by_listeners(rows) {
            if taken >= proposed::ALBUMS_PER_ARTIST {
                break;
            }
            let group = row.album.release_group_mbid.trim().to_ascii_lowercase();
            if group.is_empty()
                || seen.contains(&group)
                || !gate::may_offer(&row.album, &linked.mbid)
                || !gates.admits(&group, &linked.mbid)
            {
                continue;
            }
            seen.push(group);
            found.push(rank::album_candidate(
                row,
                (&linked.mbid, &linked.name, linked.listeners),
                CandidateKind::Thread,
                (
                    std::slice::from_ref(&path),
                    linked.strength,
                    linked.group,
                    linked.strength,
                ),
                gates.artists,
            ));
            taken += 1;
        }
    }
    found
}
