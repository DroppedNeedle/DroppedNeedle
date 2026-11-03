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
//!   all read this one meaning. A release group is owned by the one
//!   definition catalog and acquisition share,
//!   [`droppedneedle_library::owns_group`]. An artist is owned when it is
//!   live and credited on an owned album. Being identified (`linked`) is not
//!   ownership.
//! - **Recording id.** A track's recording id in an answer is its match
//!   row's, else the one in the file's tags: one definition,
//!   [`droppedneedle_library::recording_mbid`], which media and acquire
//!   read too. It is shown and sent, and Weekly Mix finds the library file
//!   for a recording it picked by it; it is never stored as identity and
//!   never used to identify or own anything. The web player sends it with a
//!   play through another server or a YouTube link, and such a play is
//!   filed by the id it carries (`library/src/repairs/plays.rs`).
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

/// The provider every identity join names, over an identity alias.
macro_rules! musicbrainz {
    ($identity:literal) => {
        concat!($identity, ".provider = 'musicbrainz'")
    };
}

/// A track's format as a person or a browser needs it: an MP4 file is AAC or
/// ALAC, and the scan reads a bit depth only from ALAC.
macro_rules! track_codec {
    ($track:literal) => {
        concat!(
            "CASE WHEN ",
            $track,
            ".file_format = 'm4a' AND ",
            $track,
            ".bit_depth IS NOT NULL THEN 'alac' ELSE ",
            $track,
            ".file_format END"
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
            $crate::local::definitions::owned_album!("ob"),
            ")"
        )
    };
}

pub(crate) use {live_album, live_track, musicbrainz, owned_album, owned_artist, track_codec};

/// Album `$album`'s primary format: its most common indexed track format,
/// lowercased; NULL for an album without indexed tracks.
macro_rules! album_primary_format {
    ($album:literal) => {
        concat!(
            "LOWER((SELECT t2.file_format FROM local_tracks t2 WHERE t2.local_album_id = ",
            $album,
            ".id AND t2.availability = 'indexed' \
             GROUP BY t2.file_format ORDER BY COUNT(*) DESC, t2.file_format LIMIT 1))"
        )
    };
}

pub(crate) use album_primary_format;
