//! One page of a list, and the cursor that asks for the next.
//!
//! A paged list takes `limit` and `cursor` and answers [`Page`]. The cursor
//! is the list's own key for where the next page starts (an offset, or the
//! last row's sort key), written as base64 JSON so callers pass it back
//! unread. A limit above the list's maximum is clamped to it; a cursor that
//! does not read as the list's key is a 400.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Serialize, de::DeserializeOwned};
use utoipa::ToSchema;

use crate::error::ApiError;

const CURSOR_INVALID: &str = "That cursor is not valid; start from the first page.";

/// The key of a list sorted by a time, ties broken by id.
pub type TimeKey = (f64, String);

/// One page of a list.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Pass back as `cursor` for the next page; null on the last.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    /// Rows in the whole list, for lists that count them.
    #[schema(required = true)]
    pub total: Option<u64>,
}

impl<T> Page<T> {
    /// A page whose next page starts at `next`; `None` ends the list.
    pub fn new<K: Serialize>(items: Vec<T>, next: Option<K>) -> Self {
        Self {
            items,
            next_cursor: next.as_ref().and_then(encode),
            total: None,
        }
    }

    /// A page cut from rows read with `limit + 1`: the extra row only says
    /// another page follows, which starts after the last row kept.
    pub fn cut<K: Serialize>(mut rows: Vec<T>, limit: usize, key: impl FnOnce(&T) -> K) -> Self {
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = if more { rows.last().map(key) } else { None };
        Self::new(rows, next)
    }

    /// A page of a list read by offset: the rows from `offset` up to
    /// `limit` of them, of `total` in the list. The next page starts
    /// `limit` rows on, even where a row read did not make an item.
    pub fn at_offset(items: Vec<T>, offset: u64, limit: u64, total: u64) -> Self {
        let end = offset.saturating_add(limit);
        Self::new(items, (end < total).then_some(end)).with_total(total)
    }

    pub fn with_total(mut self, total: u64) -> Self {
        self.total = Some(total);
        self
    }

    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            next_cursor: self.next_cursor,
            total: self.total,
        }
    }
}

/// A list's `limit` and `cursor`, read.
#[derive(Debug, Clone, PartialEq)]
pub struct PageRequest<K> {
    /// From 1 to the list's maximum.
    pub limit: usize,
    /// Where this page starts; `None` is the first page.
    pub after: Option<K>,
}

impl<K: DeserializeOwned> PageRequest<K> {
    /// No limit is `default`, one above `max` is `max`. An empty cursor is
    /// the first page.
    pub fn read(
        limit: Option<u32>,
        cursor: Option<&str>,
        default: usize,
        max: usize,
    ) -> Result<Self, ApiError> {
        let limit = limit.map_or(default, |limit| limit as usize).clamp(1, max);
        let after = cursor
            .filter(|raw| !raw.is_empty())
            .map(decode)
            .transpose()?;
        Ok(Self { limit, after })
    }
}

fn encode<K: Serialize>(key: &K) -> Option<String> {
    match serde_json::to_vec(key) {
        Ok(json) => Some(URL_SAFE_NO_PAD.encode(json)),
        Err(error) => {
            tracing::error!(%error, "page cursor did not encode; the list ends here");
            None
        }
    }
}

fn decode<K: DeserializeOwned>(raw: &str) -> Result<K, ApiError> {
    URL_SAFE_NO_PAD
        .decode(raw)
        .ok()
        .and_then(|json| serde_json::from_slice(&json).ok())
        .ok_or_else(|| ApiError::invalid_input(CURSOR_INVALID))
}
