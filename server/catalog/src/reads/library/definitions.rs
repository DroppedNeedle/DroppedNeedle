//! What a library album, "owned" and a track's recording id mean, once,
//! for every catalog read.
//!
//! - **Live.** An album merged into another (`retired_into_album_id` set)
//!   is never listed, counted or returned. A lookup by its id follows the
//!   alias to the album that holds it now.
//! - **Has files.** An album has files when `has_indexed_track = 1` (kept
//!   by triggers): at least one of its tracks is indexed. Missing tracks
//!   (gone from disk or in the recycle bin) and excluded ones do not count.
//!   The web UI lists a live album without files, with no tracks; player
//!   apps never see it.
//! - **Live track.** A track every read shows is indexed and sits on a
//!   live album. Merges retire only emptied albums, so a track left on a
//!   retired one is a leftover no read lists, counts or returns.
//! - **Owned** means live and has files. Membership, search's
//!   `in_library`, the catalog pages' local fallback, Discover and follows
//!   all read this one meaning. A release group is owned when any copy of
//!   it is. An artist is owned when it is live and credited on an owned
//!   album. Being identified (`linked`) is not ownership.
//! - **Recording id.** A track's recording id in an answer is the accepted
//!   identity's, else the one in the file's tags. It is shown and sent
//!   only: never stored as identity, never used to match, file or own
//!   anything.
//! - Every join to an `*_external_identities` table names the provider.
//!
//! Each definition is a macro that expands to a string literal over the
//! aliases it is given, so it fits in `concat!` and in `const` statements.
//! The album predicates write the columns with a unary plus, so an ordered
//! page keeps walking its title or date index instead of the retired one.

/// An album that has not been merged into another, over an album alias.
macro_rules! live_album {
    ($album:literal) => {
        concat!("+", $album, ".retired_into_album_id IS NULL")
    };
}

/// An indexed track on a live album, over a track alias. The album is
/// probed by key per track, so a page keeps walking its own index.
macro_rules! live_track {
    ($track:literal) => {
        concat!(
            $track,
            ".availability = 'indexed' AND NOT EXISTS (SELECT 1 FROM local_albums ra \
             WHERE ra.id = ",
            $track,
            ".local_album_id AND +ra.retired_into_album_id IS NOT NULL)"
        )
    };
}

/// An album that is live and has at least one indexed track, over an
/// album alias.
macro_rules! owned_album {
    ($album:literal) => {
        concat!(
            "+",
            $album,
            ".retired_into_album_id IS NULL AND +",
            $album,
            ".has_indexed_track = 1"
        )
    };
}

/// The order a lookup by release group picks a copy in, over an album
/// alias: the oldest copy with files, else the oldest copy.
macro_rules! copy_order {
    ($album:literal) => {
        concat!(
            $album,
            ".has_indexed_track DESC, ",
            $album,
            ".created_at ASC, ",
            $album,
            ".id ASC"
        )
    };
}

/// The provider every identity join names, over an identity alias.
macro_rules! musicbrainz {
    ($identity:literal) => {
        concat!($identity, ".provider = 'musicbrainz'")
    };
}

/// A track's recording id: the accepted identity's, else the file's own,
/// over an identity alias and a track alias.
macro_rules! recording_mbid {
    ($identity:literal, $track:literal) => {
        concat!(
            "COALESCE(",
            $identity,
            ".recording_mbid, ",
            $track,
            ".embedded_recording_mbid)"
        )
    };
}

/// Every owned release group, lowercased: a subquery for `IN (...)`.
macro_rules! owned_release_groups {
    () => {
        concat!(
            "SELECT lower(oe.release_group_mbid) FROM local_album_external_identities oe \
             JOIN local_albums ob ON ob.id = oe.local_album_id WHERE ",
            $crate::reads::library::definitions::musicbrainz!("oe"),
            " AND ",
            $crate::reads::library::definitions::owned_album!("ob")
        )
    };
}

/// An owned artist, over an artist alias: live and credited on an owned
/// album.
macro_rules! owned_artist {
    ($artist:literal) => {
        concat!(
            $artist,
            ".retired_into_artist_id IS NULL AND EXISTS (SELECT 1 FROM local_album_artists oc \
             JOIN local_albums ob ON ob.id = oc.local_album_id WHERE oc.local_artist_id = ",
            $artist,
            ".id AND ",
            $crate::reads::library::definitions::owned_album!("ob"),
            ")"
        )
    };
}

pub(crate) use {
    copy_order, live_album, live_track, musicbrainz, owned_album, owned_artist,
    owned_release_groups, recording_mbid,
};
