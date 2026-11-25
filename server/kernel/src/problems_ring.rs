//! Recent problems: the server's latest WARN and ERROR events, kept in a
//! bounded ring in memory so an admin can find one by the ref a person
//! gives them.
//!
//! The ring is one more layer of the one subscriber ([`super::observability`]),
//! under its own WARN filter, so it fills whatever `LOG_LEVEL` says. Repeats
//! of one callsite with the same message and `cause` within ten minutes fold
//! into one entry. Each entry also keeps, from the moment it is written, the
//! redacted form a support bundle may carry, so the bundle never depends on
//! a later filter.
//!
//! Limits: 500 entries and 1 MiB held, counted by the bytes of the raw and
//! redacted forms together; each value cut at 256 bytes on a character
//! boundary; an entry at 2 KiB (fields past it are dropped and counted); 16
//! refs an entry; 50 entries a callsite, so one noisy source cannot flush
//! the rest. Times are `Instant`s, turned into wall time when read. Nothing
//! calls tracing while the ring's lock is held, and a poisoned lock is
//! taken back.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Level, Subscriber, callsite::Identifier};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Entries kept at most.
pub const MAX_ENTRIES: usize = 500;
/// Bytes held at most, raw and redacted forms together.
pub const MAX_BYTES: usize = 1024 * 1024;
/// Bytes one value keeps.
pub const MAX_VALUE_BYTES: usize = 256;
/// Bytes one entry holds at most.
pub const MAX_ENTRY_BYTES: usize = 2 * 1024;
/// Refs one entry keeps.
pub const MAX_REFS: usize = 16;
/// Entries one callsite keeps.
pub const MAX_PER_CALLSITE: usize = 50;
/// How long a repeat folds into the entry before it.
pub const FOLD_WINDOW: Duration = Duration::from_secs(10 * 60);

/// String fields the redacted form keeps, when their value is a plain token.
const SAFE_TEXT_FIELDS: &[&str] = &["code", "operation", "lane", "kind", "provider"];
/// Number and switch fields the redacted form keeps.
const SAFE_NUMBER_FIELDS: &[&str] = &[
    "attempt",
    "status",
    "count",
    "elapsed_ms",
    "rows",
    "retry_after_secs",
];
/// Fields whose value is the request's ref.
const REF_FIELDS: &[&str] = &["request_id"];

/// An event's level, as the ring keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemLevel {
    /// A warning.
    Warn,
    /// An error.
    Error,
}

/// One field's value as the event gave it.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// Text, cut to [`MAX_VALUE_BYTES`].
    Text(String),
    /// A whole number.
    Int(i64),
    /// A float.
    Float(f64),
    /// A switch.
    Bool(bool),
}

impl FieldValue {
    fn bytes(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            _ => 8,
        }
    }
}

/// The form of an entry a support bundle may carry: no message, no text a
/// person or a server wrote, a file cut to its crate, only the fields on the
/// allowlists, and only refs shaped like the ids the server mints.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Redacted {
    /// The file, cut to `<crate dir>/src/...`; `None` when it would still
    /// name an absolute path.
    pub file: Option<String>,
    /// Allowlisted fields.
    pub fields: Vec<(&'static str, FieldValue)>,
    /// Refs with the minted id's shape.
    pub refs: Vec<String>,
    /// Refs of another shape, counted and never shown.
    pub other_refs: u32,
}

/// One kept problem.
#[derive(Debug, Clone)]
pub struct Entry {
    /// When it was first and last seen.
    pub first: Instant,
    /// The newest repeat.
    pub last: Instant,
    /// Its level.
    pub level: ProblemLevel,
    /// Its target (module path).
    pub target: &'static str,
    /// Its file, as the build recorded it.
    pub file: Option<&'static str>,
    /// Its line.
    pub line: Option<u32>,
    /// Its message.
    pub message: String,
    /// Its other fields, refs included.
    pub fields: Vec<(&'static str, FieldValue)>,
    /// Fields left out to keep the entry under [`MAX_ENTRY_BYTES`].
    pub dropped_fields: u32,
    /// How many times it was seen.
    pub count: u64,
    /// The requests it was seen in.
    pub refs: Vec<String>,
    /// What a support bundle may carry of it.
    pub redacted: Redacted,
    callsite: Identifier,
    cause: Option<String>,
    bytes: usize,
}

impl Entry {
    /// Whether one of its refs starts with `reference`.
    pub fn has_ref(&self, reference: &str) -> bool {
        self.refs.iter().any(|found| found.starts_with(reference))
    }
}

#[derive(Default)]
struct Ring {
    entries: VecDeque<Entry>,
    bytes: usize,
}

/// The ring, shared by its layer and the admin's reads. Built once, in
/// `main` for the server and by the test app in tests, and passed in.
#[derive(Clone, Default)]
pub struct RecentProblems {
    ring: Arc<Mutex<Ring>>,
}

impl fmt::Debug for RecentProblems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecentProblems").finish_non_exhaustive()
    }
}

impl RecentProblems {
    /// An empty ring.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The kept entries, newest first, only those with a ref starting with
    /// `reference` when one is given.
    pub fn entries(&self, reference: Option<&str>) -> Vec<Entry> {
        let mut entries: Vec<Entry> = self
            .lock()
            .entries
            .iter()
            .filter(|entry| reference.is_none_or(|found| entry.has_ref(found)))
            .cloned()
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.last));
        entries
    }

    /// The ring as one more layer of the one subscriber, under a WARN
    /// filter of its own, so it fills whatever the log's level is.
    pub fn layer(&self) -> crate::observability::ExtraLayer {
        self.clone().with_filter(LevelFilter::WARN).boxed()
    }

    /// Bytes held now.
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }

    fn record(&self, seen: Seen) {
        let now = Instant::now();
        let mut ring = self.lock();
        let ring = &mut *ring;
        if let Some(entry) = ring.entries.iter_mut().rev().find(|entry| {
            entry.callsite == seen.callsite
                && entry.message == seen.message
                && entry.cause == seen.cause
                && now.duration_since(entry.last) <= FOLD_WINDOW
        }) {
            entry.count += 1;
            entry.last = now;
            for reference in seen.refs {
                if entry.refs.len() >= MAX_REFS || entry.refs.contains(&reference) {
                    continue;
                }
                if entry.bytes + reference.len() * 2 > MAX_ENTRY_BYTES {
                    break;
                }
                let added = add_ref(&mut entry.redacted, &reference);
                entry.bytes += reference.len() + added;
                ring.bytes += reference.len() + added;
                entry.refs.push(reference);
            }
        } else {
            let entry = seen.into_entry(now);
            let same = ring
                .entries
                .iter()
                .filter(|kept| kept.callsite == entry.callsite)
                .count();
            if same >= MAX_PER_CALLSITE
                && let Some(at) = ring
                    .entries
                    .iter()
                    .position(|kept| kept.callsite == entry.callsite)
                && let Some(gone) = ring.entries.remove(at)
            {
                ring.bytes -= gone.bytes;
            }
            ring.bytes += entry.bytes;
            ring.entries.push_back(entry);
        }
        while ring.entries.len() > MAX_ENTRIES || ring.bytes > MAX_BYTES {
            let Some(gone) = ring.entries.pop_front() else {
                break;
            };
            ring.bytes -= gone.bytes;
        }
    }
}

/// What one event gave, before it is folded or kept.
struct Seen {
    callsite: Identifier,
    level: ProblemLevel,
    target: &'static str,
    file: Option<&'static str>,
    line: Option<u32>,
    message: String,
    cause: Option<String>,
    fields: Vec<(&'static str, FieldValue)>,
    refs: Vec<String>,
}

impl Seen {
    fn into_entry(self, now: Instant) -> Entry {
        let mut bytes = self.message.len()
            + self.target.len()
            + self.file.map_or(0, str::len)
            + self.cause.as_deref().map_or(0, str::len);
        let mut redacted = Redacted {
            file: self.file.and_then(cut_file),
            ..Redacted::default()
        };
        bytes += redacted.file.as_deref().map_or(0, str::len);
        let mut fields = Vec::new();
        let mut dropped_fields = 0;
        for (name, value) in self.fields {
            let size = name.len() + value.bytes();
            if bytes + size * 2 > MAX_ENTRY_BYTES {
                dropped_fields += 1;
                continue;
            }
            bytes += size;
            if let Some(safe) = redact_field(name, &value) {
                bytes += size;
                redacted.fields.push((name, safe));
            }
            fields.push((name, value));
        }
        let mut refs = Vec::new();
        for reference in self.refs {
            if refs.len() >= MAX_REFS || refs.contains(&reference) {
                continue;
            }
            if bytes + reference.len() * 2 > MAX_ENTRY_BYTES {
                break;
            }
            bytes += reference.len() + add_ref(&mut redacted, &reference);
            refs.push(reference);
        }
        Entry {
            first: now,
            last: now,
            level: self.level,
            target: self.target,
            file: self.file,
            line: self.line,
            message: self.message,
            fields,
            dropped_fields,
            count: 1,
            refs,
            redacted,
            callsite: self.callsite,
            cause: self.cause,
            bytes,
        }
    }
}

/// Add `reference` to the redacted refs when it has the minted id's shape,
/// else count it. Answers the bytes the redacted form grew by.
fn add_ref(redacted: &mut Redacted, reference: &str) -> usize {
    if is_minted_id(reference) {
        if redacted.refs.iter().any(|kept| kept == reference) {
            return 0;
        }
        redacted.refs.push(reference.to_owned());
        reference.len()
    } else {
        redacted.other_refs += 1;
        0
    }
}

/// The hyphenated lowercase UUID the server's id generator makes. A ref a
/// client sent in `x-request-id` may be any short token, such as a name, so
/// only this shape is ever shown outside the admin's own view.
pub fn is_minted_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        })
}

/// An error code: `^[A-Z][A-Z0-9_]{0,63}$`.
pub fn is_error_code(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_uppercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
}

/// A plain token: letters, digits, `_`, `.`, `:` and `-`, 64 at most.
pub fn is_safe_token(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
}

/// The redacted value of one field, when its name is on an allowlist.
fn redact_field(name: &str, value: &FieldValue) -> Option<FieldValue> {
    let numeric_name = SAFE_NUMBER_FIELDS.contains(&name)
        && !["id", "port", "uid", "gid"]
            .iter()
            .any(|end| name.ends_with(end));
    match value {
        // A `code` is an error code, SCREAMING_SNAKE, never a sign-in code.
        FieldValue::Text(text) if name == "code" && is_error_code(text) => Some(value.clone()),
        FieldValue::Text(text)
            if name != "code" && SAFE_TEXT_FIELDS.contains(&name) && is_safe_token(text) =>
        {
            Some(value.clone())
        }
        FieldValue::Int(_) | FieldValue::Float(_) | FieldValue::Bool(_) if numeric_name => {
            Some(value.clone())
        }
        _ => None,
    }
}

/// A callsite's file cut to `<crate dir>/src/...`. A dependency's file
/// carries the registry's absolute path, which names the home folder: the
/// text after `/registry/src/<index>/` is kept. A workspace file is already
/// relative. Anything still absolute is dropped.
pub fn cut_file(file: &str) -> Option<String> {
    let cut = match file.find("/registry/src/") {
        Some(at) => {
            let rest = &file[at + "/registry/src/".len()..];
            rest.split_once('/').map_or(rest, |(_, after)| after)
        }
        None => file,
    };
    (!cut.starts_with('/') && !cut.is_empty()).then(|| cut.to_owned())
}

/// `text` cut to [`MAX_VALUE_BYTES`] on a character boundary.
fn cut(mut text: String) -> String {
    if text.len() > MAX_VALUE_BYTES {
        let mut end = MAX_VALUE_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// Gathers an event's or a span's fields.
#[derive(Default)]
struct Fields {
    message: Option<String>,
    cause: Option<String>,
    fields: Vec<(&'static str, FieldValue)>,
    refs: Vec<String>,
}

impl Fields {
    fn text(&mut self, field: &Field, text: String) {
        let text = cut(text);
        match field.name() {
            "message" => self.message = Some(text),
            name => {
                if REF_FIELDS.contains(&name) {
                    self.refs.push(text.clone());
                }
                if name == "cause" {
                    self.cause = Some(text.clone());
                }
                self.fields.push((name, FieldValue::Text(text)));
            }
        }
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.text(field, value.to_owned());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.push((field.name(), FieldValue::Int(value)));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.push((
            field.name(),
            FieldValue::Int(i64::try_from(value).unwrap_or(i64::MAX)),
        ));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields.push((field.name(), FieldValue::Float(value)));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.push((field.name(), FieldValue::Bool(value)));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.text(field, format!("{value:?}"));
    }
}

/// The ref a request span carries, kept in the span's extensions.
struct SpanRef(String);

impl<S> Layer<S> for RecentProblems
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        if let Some(reference) = fields.refs.into_iter().next()
            && let Some(span) = ctx.span(id)
        {
            span.extensions_mut().insert(SpanRef(reference));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            Level::ERROR => ProblemLevel::Error,
            Level::WARN => ProblemLevel::Warn,
            _ => return,
        };
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut refs = fields.refs;
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope {
                if let Some(found) = span.extensions().get::<SpanRef>() {
                    refs.push(found.0.clone());
                }
            }
        }
        self.record(Seen {
            callsite: metadata.callsite(),
            level,
            target: metadata.target(),
            file: metadata.file(),
            line: metadata.line(),
            message: fields.message.unwrap_or_default(),
            cause: fields.cause,
            fields: fields.fields,
            refs,
        });
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    fn with_ring(ring: &RecentProblems, run: impl FnOnce()) {
        // The log writer at error, the ring at warn: per-layer filters.
        let subscriber = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::sink)
                    .with_filter(LevelFilter::ERROR),
            )
            .with(ring.clone().with_filter(LevelFilter::WARN));
        tracing::subscriber::with_default(subscriber, run);
    }

    #[test]
    fn a_warning_reaches_the_ring_whatever_the_log_level() {
        let ring = RecentProblems::new();
        with_ring(&ring, || {
            tracing::info!("not kept");
            tracing::warn!(code = "SOME_CODE", "kept");
        });
        let entries = ring.entries(None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "kept");
        assert_eq!(entries[0].level, ProblemLevel::Warn);
    }

    #[test]
    fn repeats_fold_and_refs_find_them() {
        let ring = RecentProblems::new();
        let minted = "4f1c2a9e-7d3b-4c1a-9e2f-0123456789ab";
        with_ring(&ring, || {
            for request_id in [minted, "alice-laptop"] {
                tracing::error!(request_id, cause = "disk", "request failed");
            }
        });
        let entries = ring.entries(Some("4f1c2a9e"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].count, 2);
        assert_eq!(entries[0].redacted.refs, vec![minted.to_owned()]);
        assert_eq!(entries[0].redacted.other_refs, 1);
        assert!(ring.entries(Some("nothing")).is_empty());
    }

    #[test]
    fn the_ring_keeps_to_its_bounds() {
        let ring = RecentProblems::new();
        let long = "x".repeat(10 * 1024);
        with_ring(&ring, || {
            for n in 0..2_000 {
                tracing::warn!(cause = %n, detail = %long, "noisy");
            }
        });
        let entries = ring.entries(None);
        assert!(entries.len() <= MAX_PER_CALLSITE, "{}", entries.len());
        assert!(ring.bytes() <= MAX_BYTES);
        let detail = entries[0]
            .fields
            .iter()
            .find(|(name, _)| *name == "detail")
            .map(|(_, value)| value.clone());
        assert_eq!(detail, Some(FieldValue::Text("x".repeat(MAX_VALUE_BYTES))));
    }

    #[test]
    fn only_an_error_code_passes_as_a_code() {
        let ring = RecentProblems::new();
        with_ring(&ring, || {
            tracing::warn!(code = "BACKUP_NO_ROOM", "an error code");
            tracing::warn!(code = "4F1C-2A9E", "a code someone signs in with");
            tracing::warn!(code = "abcd-1234", "another shape");
        });
        let code_of = |message: &str| {
            ring.entries(None)
                .into_iter()
                .find(|entry| entry.message == message)
                .and_then(|entry| {
                    entry
                        .redacted
                        .fields
                        .into_iter()
                        .find(|(name, _)| *name == "code")
                        .map(|(_, value)| value)
                })
        };
        assert_eq!(
            code_of("an error code"),
            Some(FieldValue::Text("BACKUP_NO_ROOM".to_owned()))
        );
        assert_eq!(code_of("a code someone signs in with"), None);
        assert_eq!(code_of("another shape"), None);
    }

    #[test]
    fn files_are_cut_to_their_crate() {
        assert_eq!(
            cut_file("/home/someone/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/hyper-1.5.0/src/proto.rs")
                .as_deref(),
            Some("hyper-1.5.0/src/proto.rs")
        );
        assert_eq!(
            cut_file("kernel/src/db/writer.rs").as_deref(),
            Some("kernel/src/db/writer.rs")
        );
        assert_eq!(cut_file("/home/someone/elsewhere/lib.rs"), None);
    }
}
