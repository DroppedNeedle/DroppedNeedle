//! Ordered response values plus the XML/JSON/JSONP serializer.
//!
//! v2: `backend/api/compat/subsonic/serialization.py`. The value model is
//! ordered (field order on the wire matches v2's `msgspec.to_builtins`
//! struct order): objects render scalars as attributes in order, a `value`
//! key as text, objects as child elements, and lists as repeated children.
//!
//! Rules ported verbatim: `None` stripped recursively (empty lists and
//! empty objects stay); XML booleans lowercase; C0 controls stripped
//! from XML text; JSONP callbacks allowlisted to a bare identifier path
//! (unsafe or overlong falls back to JSON); XML prolog + namespace.

use super::error::{NOT_AUTHORIZED, NOT_FOUND, SubsonicError};

/// Pinned protocol version, in every envelope.
pub const API_VERSION: &str = "1.16.1";
/// Envelope namespace.
pub const NAMESPACE: &str = "http://subsonic.org/restapi";
/// Advertised server type when no name is configured.
pub const DEFAULT_SERVER_NAME: &str = "DroppedNeedle";

/// An ordered response value. Built by the models; [`Val::stripped`]
/// removes nulls before rendering.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// Removed by [`Val::stripped`]; never rendered.
    Null,
    /// XML attr/text `true`/`false`; JSON literal.
    Bool(bool),
    /// Integer scalar.
    Int(i64),
    /// Float scalar (replay gain, lyric offset).
    Float(f64),
    /// String scalar.
    Str(String),
    /// Repeated `<tag>` children / JSON array.
    List(Vec<Val>),
    /// Element / JSON object, insertion order kept.
    Obj(Vec<(String, Val)>),
}

impl Val {
    /// Recursively drop nulls; empty lists/objects survive (v2 parity).
    pub fn stripped(&self) -> Val {
        match self {
            Val::Obj(entries) => Val::Obj(
                entries
                    .iter()
                    .filter(|(_, value)| !matches!(value, Val::Null))
                    .map(|(key, value)| (key.clone(), value.stripped()))
                    .collect(),
            ),
            Val::List(items) => Val::List(
                items
                    .iter()
                    .map(Val::stripped)
                    .filter(|item| !matches!(item, Val::Null))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Render as compact JSON (nulls already stripped by the caller).
    pub fn to_json(&self) -> String {
        match self {
            Val::Null => "null".to_owned(),
            Val::Bool(true) => "true".to_owned(),
            Val::Bool(false) => "false".to_owned(),
            Val::Int(n) => n.to_string(),
            Val::Float(f) => format_float(*f),
            Val::Str(s) => format!("\"{}\"", json_escape(s)),
            Val::List(items) => {
                let inner: Vec<String> = items.iter().map(Val::to_json).collect();
                format!("[{}]", inner.join(","))
            }
            Val::Obj(entries) => {
                let inner: Vec<String> = entries
                    .iter()
                    .map(|(key, value)| format!("\"{}\":{}", json_escape(key), value.to_json()))
                    .collect();
                format!("{{{}}}", inner.join(","))
            }
        }
    }

    /// Render as an XML element `tag` (v2 `_to_xml`).
    pub fn to_xml(&self, tag: &str) -> String {
        match self {
            Val::Obj(entries) => {
                let mut attrs = String::new();
                let mut text = String::new();
                let mut inner = String::new();
                for (key, value) in entries {
                    if key == "value" {
                        text = xml_escape_text(&scalar_text(value));
                    } else if let Val::Obj(_) = value {
                        inner.push_str(&value.to_xml(key));
                    } else if let Val::List(items) = value {
                        for item in items {
                            inner.push_str(&item.to_xml(key));
                        }
                    } else {
                        attrs.push_str(&format!(
                            " {key}=\"{}\"",
                            xml_escape_attr(&scalar_text(value))
                        ));
                    }
                }
                inner = format!("{text}{inner}");
                if inner.is_empty() {
                    format!("<{tag}{attrs}/>")
                } else {
                    format!("<{tag}{attrs}>{inner}</{tag}>")
                }
            }
            Val::List(items) => {
                let mut out = String::new();
                for item in items {
                    out.push_str(&item.to_xml(tag));
                }
                out
            }
            scalar => format!("<{tag}>{}</{tag}>", xml_escape_text(&scalar_text(scalar))),
        }
    }
}

/// Scalar to text: bools lowercase (v2 `_xml_scalar`).
fn scalar_text(value: &Val) -> String {
    match value {
        Val::Bool(true) => "true".to_owned(),
        Val::Bool(false) => "false".to_owned(),
        Val::Int(n) => n.to_string(),
        Val::Float(f) => format_float(*f),
        Val::Str(s) => s.clone(),
        Val::Null => String::new(),
        Val::Obj(_) | Val::List(_) => String::new(),
    }
}

fn format_float(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Strip C0 controls illegal in XML 1.0 (except tab/newline/CR), then
/// escape text (v2 `_xml_escape_text`).
fn xml_escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}') {
            continue;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

fn xml_escape_attr(value: &str) -> String {
    xml_escape_text(value).replace('"', "&quot;")
}

/// Build an object value from key/value pairs.
pub fn obj(entries: Vec<(&str, Val)>) -> Val {
    Val::Obj(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

/// Conversion into [`Val`].
pub trait IntoVal {
    /// Convert into a value.
    fn into_val(self) -> Val;
}

impl IntoVal for Val {
    fn into_val(self) -> Val {
        self
    }
}

impl IntoVal for bool {
    fn into_val(self) -> Val {
        Val::Bool(self)
    }
}

impl IntoVal for i64 {
    fn into_val(self) -> Val {
        Val::Int(self)
    }
}

impl IntoVal for i32 {
    fn into_val(self) -> Val {
        Val::Int(i64::from(self))
    }
}

impl IntoVal for usize {
    fn into_val(self) -> Val {
        Val::Int(self as i64)
    }
}

impl IntoVal for f64 {
    fn into_val(self) -> Val {
        Val::Float(self)
    }
}

impl IntoVal for String {
    fn into_val(self) -> Val {
        Val::Str(self)
    }
}

impl IntoVal for &str {
    fn into_val(self) -> Val {
        Val::Str(self.to_owned())
    }
}

impl<T: IntoVal> IntoVal for Option<T> {
    fn into_val(self) -> Val {
        self.map(IntoVal::into_val).unwrap_or(Val::Null)
    }
}

impl<T: IntoVal> IntoVal for Vec<T> {
    fn into_val(self) -> Val {
        Val::List(self.into_iter().map(IntoVal::into_val).collect())
    }
}

/// Response format from the `f` param (default XML).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubsonicFormat {
    /// XML (default).
    #[default]
    Xml,
    /// JSON.
    Json,
    /// JSONP (falls back to JSON on a missing/unsafe callback).
    Jsonp,
}

/// Parse the `f` param; absent means XML, anything else is code 10.
pub fn parse_format(value: Option<&str>) -> Result<SubsonicFormat, SubsonicError> {
    match value {
        None => Ok(SubsonicFormat::Xml),
        Some("xml") => Ok(SubsonicFormat::Xml),
        Some("json") => Ok(SubsonicFormat::Json),
        Some("jsonp") => Ok(SubsonicFormat::Jsonp),
        Some(_) => Err(SubsonicError::invalid("f")),
    }
}

/// A callback is safe only as a bare identifier path of at most 128
/// chars, else the response falls back to JSON (v2 `_CALLBACK_RE`:
/// `^[A-Za-z_$][\w$.]{0,127}$`).
pub fn callback_is_safe(callback: &str) -> bool {
    let mut chars = callback.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    if callback.chars().count() > 128 {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.')
}

/// One rendered HTTP response: status, content type, exact body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// HTTP status (200 for envelopes, 403/404/416/429 for binary).
    pub status: u16,
    /// Content type without charset suffix.
    pub content_type: String,
    /// Extra headers (cache control, content range, ...).
    pub headers: Vec<(String, String)>,
    /// Exact body bytes.
    pub body: Vec<u8>,
}

impl Rendered {
    /// Body as UTF-8 lossy text (envelopes are ASCII-safe).
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// First header value for a case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Envelope attributes shared by ok and failed responses (v2 `_envelope`).
fn envelope(status: &str, server_name: &str, server_version: &str) -> Vec<(&'static str, Val)> {
    vec![
        ("status", status.into_val()),
        ("version", API_VERSION.into_val()),
        ("type", server_name.into_val()),
        ("serverVersion", server_version.into_val()),
        ("openSubsonic", true.into_val()),
    ]
}

/// Emit the body in the requested format (v2 `_emit`).
fn emit(body: Val, format: SubsonicFormat, callback: Option<&str>) -> Rendered {
    let safe_callback = callback.filter(|cb| callback_is_safe(cb));
    match format {
        SubsonicFormat::Xml => {
            // Envelopes always build objects; a non-object renders with no
            // attributes rather than panicking (panics are denied here).
            let entries = match body {
                Val::Obj(entries) => entries,
                _ => Vec::new(),
            };
            let mut root = vec![("xmlns".to_owned(), Val::Str(NAMESPACE.to_owned()))];
            root.extend(entries);
            let xml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>{}",
                Val::Obj(root).to_xml("subsonic-response")
            );
            Rendered {
                status: 200,
                content_type: "application/xml".to_owned(),
                headers: Vec::new(),
                body: xml.into_bytes(),
            }
        }
        SubsonicFormat::Json => Rendered {
            status: 200,
            content_type: "application/json".to_owned(),
            headers: Vec::new(),
            body: obj(vec![("subsonic-response", body)])
                .to_json()
                .into_bytes(),
        },
        SubsonicFormat::Jsonp => {
            let payload = obj(vec![("subsonic-response", body)]).to_json();
            match safe_callback {
                Some(cb) => Rendered {
                    status: 200,
                    content_type: "application/javascript".to_owned(),
                    headers: Vec::new(),
                    body: format!("{cb}({payload});").into_bytes(),
                },
                None => Rendered {
                    status: 200,
                    content_type: "application/json".to_owned(),
                    headers: Vec::new(),
                    body: payload.into_bytes(),
                },
            }
        }
    }
}

/// Render an `ok` envelope (v2 `render`).
pub fn render_ok(
    endpoint_key: Option<&str>,
    payload: Option<Val>,
    format: SubsonicFormat,
    callback: Option<&str>,
    server_name: &str,
    server_version: &str,
) -> Rendered {
    let mut entries = envelope("ok", server_name, server_version);
    if let (Some(key), Some(value)) = (endpoint_key, payload) {
        entries.push((key, value));
    }
    emit(obj(entries).stripped(), format, callback)
}

/// Render a `failed` envelope over HTTP 200 (v2 `render_error`).
pub fn render_error(
    code: u8,
    message: &str,
    format: SubsonicFormat,
    callback: Option<&str>,
    server_name: &str,
    server_version: &str,
) -> Rendered {
    let mut entries = envelope("failed", server_name, server_version);
    entries.push((
        "error",
        obj(vec![
            ("code", Val::Int(i64::from(code))),
            ("message", message.into_val()),
        ]),
    ));
    emit(obj(entries).stripped(), format, callback)
}

/// Render a binary-path failure as `text/plain`: 70 maps to 404, 50 to
/// 403, anything else to 404 (v2 `_binary_error`).
pub fn render_binary_error(code: u8, message: &str) -> Rendered {
    let status = match code {
        NOT_FOUND => 404,
        NOT_AUTHORIZED => 403,
        _ => 404,
    };
    Rendered {
        status,
        content_type: "text/plain".to_owned(),
        headers: Vec::new(),
        body: message.as_bytes().to_vec(),
    }
}
