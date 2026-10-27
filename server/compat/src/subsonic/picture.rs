//! Open picture links: album and artist pictures at an address that needs
//! no sign-in, `/subsonic/img?id=<id>&sig=<signature>&size=<px>`.
//!
//! Apps hand the image links of `getArtists`, `getArtistInfo2` and
//! `getAlbumInfo2` to a picture loader that sends no credentials, as
//! Navidrome's own links allow. Each link names one album or artist by its
//! Subsonic id and carries a signature made with the data key under a
//! purpose of its own, so nothing else the key signs or seals (a transcode
//! parameter, a correction token, a stored secret) is ever a valid
//! signature here, and an id nobody was given cannot be opened.
//!
//! The signature covers the kind and the id only: not the size, a user, a
//! time or the address. One album has one link, the same in every answer
//! and for every user, so an app's picture cache works, and it does not
//! expire. A link stops working when the data key is replaced, when the
//! album or artist leaves the library, and while the Subsonic API is off.
//! That is safe because every user sees the whole library; if libraries
//! ever become per user, this address must change.
//!
//! An album's link also carries its cover version as `cv`, outside the
//! signature, so the link changes when the cover does and an app fetches
//! the new picture. The server never reads it: a link with an old version,
//! a made-up one or none opens the album's current picture, so links apps
//! stored keep working.
//!
//! A refused link is an empty 404 and is not a failed sign-in, so stale
//! links never lock a household out of the API. Signatures are made only
//! by [`link`], for ids the catalog has just returned.

use std::sync::Arc;

use super::ids::{IdKind, encode};
use super::media::library_picture;
use super::stream::cover_bucket;
use super::value::Rendered;
use super::{Outcome, Settings};
use crate::adapters::subsonic_store::SubsonicStore;
use droppedneedle_auth::session::tokens::constant_time_eq;
use droppedneedle_kernel::runtime_config::Crypto;

/// The route, as registered.
pub const PATH: &str = "/subsonic/img";

/// Keeps picture signatures apart from anything else the data key signs.
pub const PURPOSE: &str = "subsonic-picture-link";

/// Pixel sizes of the small, medium and large info links.
pub const INFO_SIZES: [u32; 3] = [250, 500, 1200];

/// Pixel size of an artist's `artistImageUrl`.
pub const ARTIST_IMAGE_SIZE: u32 = 500;

/// How long an app or browser may keep an opened picture.
const CACHE_CONTROL: &str = "public, max-age=86400";

/// The picture types a link may answer with.
const IMAGE_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// What a link may show: an album's cover or an artist's picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureKind {
    /// `al-` ids.
    Album,
    /// `ar-` ids.
    Artist,
}

impl PictureKind {
    fn id_kind(self) -> IdKind {
        match self {
            Self::Album => IdKind::Album,
            Self::Artist => IdKind::Artist,
        }
    }

    /// The kind and internal id of an `al-` or `ar-` id; nothing else.
    fn parse(sid: &str) -> Option<(Self, &str)> {
        [Self::Album, Self::Artist].into_iter().find_map(|kind| {
            sid.strip_prefix(kind.id_kind().prefix())
                .filter(|internal| !internal.is_empty())
                .map(|internal| (kind, internal))
        })
    }
}

/// Signs and checks picture links with the data key.
#[derive(Clone)]
pub struct PictureSigner {
    crypto: Arc<Crypto>,
}

impl std::fmt::Debug for PictureSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PictureSigner")
    }
}

impl PictureSigner {
    /// A signer over the data key.
    pub fn new(crypto: Arc<Crypto>) -> Self {
        Self { crypto }
    }

    /// The lowercase hex signature of `kind` and `internal_id`.
    pub fn sign(&self, kind: PictureKind, internal_id: &str) -> String {
        let message = format!("{}\0{internal_id}", kind.id_kind().name());
        self.crypto
            .mac(PURPOSE, message.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Whether `sig` is the whole signature of `kind` and `internal_id`,
    /// compared in constant time.
    pub fn verifies(&self, kind: PictureKind, internal_id: &str, sig: &str) -> bool {
        constant_time_eq(&self.sign(kind, internal_id), sig)
    }
}

/// The open link to one picture under `base`, the address the app reached
/// this server at, with the album's cover version when there is one. It
/// never carries a credential.
pub fn link(
    base: &str,
    signer: &PictureSigner,
    kind: PictureKind,
    internal_id: &str,
    size: u32,
    cover_version: Option<&str>,
) -> String {
    let mut link = format!(
        "{base}{PATH}?id={}&sig={}&size={size}",
        query_escape(&encode(kind.id_kind(), internal_id)),
        signer.sign(kind, internal_id)
    );
    if let Some(version) = cover_version {
        link.push_str("&cv=");
        link.push_str(&query_escape(version));
    }
    link
}

/// Percent-encode everything but the unreserved characters.
fn query_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// What the address answers.
pub enum Opened {
    /// The picture, with its headers.
    Picture(Rendered),
    /// An empty 404: off, a bad link, or not in the library.
    Closed,
    /// The store failed (logged).
    Failed,
}

/// The one value of `key`, or `None` when it is missing or repeated.
fn only<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    let mut values = pairs
        .iter()
        .filter(|(name, _)| name == key)
        .map(|(_, value)| value.as_str());
    match (values.next(), values.next()) {
        (Some(value), None) => Some(value),
        _ => None,
    }
}

/// Answer one request for `raw_query`. The signature is checked before
/// anything is read, then one read says whether the album or artist is in
/// the library (a link never names an alias); the picture is then the one
/// `getCoverArt` serves a signed-in user, cached for a day, with `nosniff`
/// and a sandbox policy.
pub async fn open(store: &SubsonicStore, settings: &Settings, raw_query: Option<&str>) -> Opened {
    if !settings.enabled {
        return Opened::Closed;
    }
    let Ok(pairs) = super::params::decode_pairs(raw_query.unwrap_or("").as_bytes()) else {
        return Opened::Closed;
    };
    let Some((kind, internal)) = only(&pairs, "id").and_then(PictureKind::parse) else {
        tracing::debug!("picture link refused: not an album or artist id");
        return Opened::Closed;
    };
    let signed =
        only(&pairs, "sig").is_some_and(|sig| store.pictures().verifies(kind, internal, sig));
    if !signed {
        tracing::debug!("picture link refused: signature does not match");
        return Opened::Closed;
    }
    let size = only(&pairs, "size")
        .and_then(|size| size.parse::<i64>().ok())
        .filter(|size| (1..=2000).contains(size));
    match store.exists(kind.id_kind(), internal).await {
        Ok(true) => {}
        Ok(false) => return Opened::Closed,
        Err(error) => {
            tracing::error!(cause = %error, "subsonic picture read failed");
            return Opened::Failed;
        }
    }
    match library_picture(store, kind, internal, cover_bucket(size)).await {
        Ok(Some(Outcome::Binary {
            status,
            content_type,
            mut headers,
            body,
        })) if IMAGE_TYPES.contains(&content_type.as_str()) => {
            // An artist's link stays the same when its picture changes, and
            // an album's version is not checked, so a picture is cached for
            // a day, never as immutable.
            headers.retain(|(name, _)| !name.eq_ignore_ascii_case("cache-control"));
            headers.push(("Cache-Control".to_owned(), CACHE_CONTROL.to_owned()));
            headers.push(("X-Content-Type-Options".to_owned(), "nosniff".to_owned()));
            headers.push((
                "Content-Security-Policy".to_owned(),
                "sandbox; default-src 'none'".to_owned(),
            ));
            Opened::Picture(Rendered {
                status,
                content_type,
                headers,
                body,
                stream: None,
            })
        }
        Ok(_) => Opened::Closed,
        Err(error) => {
            tracing::error!(cause = %error, "subsonic picture read failed");
            Opened::Failed
        }
    }
}
