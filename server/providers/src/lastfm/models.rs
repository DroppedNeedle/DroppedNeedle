//! Last.fm wire shapes for the `track.scrobble` answer.
//!
//! Checked 2026-10-21 against https://www.last.fm/api/show/track.scrobble
//! and https://www.last.fm/api/scrobbling: a scrobble Last.fm turns away
//! still comes back as a success, with `accepted` and `ignored` counts on
//! `scrobbles` and an `ignoredMessage` code on each `scrobble` (0 none,
//! 1 artist ignored, 2 track ignored, 3 timestamp too old, 4 timestamp too
//! new, 5 daily scrobble limit exceeded). The docs show the XML form; the
//! JSON form carries the counts under `@attr` and the message text under
//! `#text`, as every other Last.fm JSON answer does. No live answer was
//! read: a scrobble needs a user's session.
//!
//! Tolerant of unknown fields, and of numbers sent as text (Last.fm sends
//! both). The `scrobbles` element and its `accepted` count are required: an
//! answer without them says nothing about the play and fails decoding.

use serde::Deserialize;

/// The whole `track.scrobble` answer.
#[derive(Debug, Deserialize)]
pub(crate) struct WireScrobbleReply {
    /// The batch's result.
    pub scrobbles: WireScrobbles,
}

/// The batch's counts and its scrobbles.
#[derive(Debug, Deserialize)]
pub(crate) struct WireScrobbles {
    /// How many were accepted and ignored.
    #[serde(rename = "@attr")]
    pub counts: WireCounts,
    /// One scrobble, or a list of them.
    #[serde(default)]
    pub scrobble: Option<OneOrMany<WireScrobble>>,
}

/// The batch's counts.
#[derive(Debug, Deserialize)]
pub(crate) struct WireCounts {
    /// Scrobbles Last.fm kept.
    pub accepted: Lenient,
}

/// A single element or a list of them; Last.fm sends a lone item bare.
/// The list is tried first: a struct also decodes from a JSON array.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum OneOrMany<T> {
    /// Several items.
    Many(Vec<T>),
    /// One item.
    One(T),
}

impl<T> OneOrMany<T> {
    /// The first item, if any.
    pub fn first(&self) -> Option<&T> {
        match self {
            Self::Many(items) => items.first(),
            Self::One(item) => Some(item),
        }
    }
}

/// One scrobble's outcome.
#[derive(Debug, Deserialize)]
pub(crate) struct WireScrobble {
    /// Why it was turned away, when it was.
    #[serde(rename = "ignoredMessage", default)]
    pub ignored_message: Option<WireIgnored>,
}

/// The reason a scrobble was turned away.
#[derive(Debug, Deserialize)]
pub(crate) struct WireIgnored {
    /// The ignored code; 0 means it was not.
    #[serde(default)]
    pub code: Option<Lenient>,
}

/// A whole number sent as a number or as text.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Lenient {
    /// Sent as a number.
    Number(i64),
    /// Sent as text.
    Text(String),
}

impl Lenient {
    /// The number, when the text holds one.
    pub fn value(&self) -> Option<i64> {
        match self {
            Self::Number(number) => Some(*number),
            Self::Text(text) => text.trim().parse().ok(),
        }
    }
}
