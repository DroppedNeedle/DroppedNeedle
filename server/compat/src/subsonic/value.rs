//! The Subsonic envelope and its JSON, JSONP and XML writers.
//!
//! An answer is serialized once, from the models' `Serialize` derives, as
//! JSON text whose keys keep the structs' field order. JSON is that text
//! and JSONP wraps it. XML walks the same text: in an object, scalars
//! become attributes in order, a `value` key becomes the element's text,
//! objects become child elements and arrays become repeated children.
//! Null keys are left out of both.
//!
//! Rules kept from v2: XML booleans lowercase; C0 controls dropped from
//! XML; JSONP callbacks allowlisted to a bare identifier path (unsafe or
//! overlong falls back to JSON); XML prolog and namespace. Floats are
//! written as v2 wrote them: a whole value keeps one decimal place.

use std::borrow::Cow;
use std::fmt;
use std::io;

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::ser::{CharEscape, CompactFormatter, Formatter};
use serde_json::value::RawValue;

use super::error::{NOT_AUTHORIZED, NOT_FOUND, ServerFault, SubsonicError};

/// Pinned protocol version, in every envelope.
pub const API_VERSION: &str = "1.16.1";
/// Envelope namespace.
pub const NAMESPACE: &str = "http://subsonic.org/restapi";

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

/// The format and callback a failure is answered in, read before strict
/// decoding: a unique valid `f` value and a unique `callback` of at most
/// 128 bytes, else XML and no callback (v2 reads a unique valid query
/// value only).
pub fn sniff_format(f: &[&str], callback: &[&str]) -> (SubsonicFormat, Option<String>) {
    let format = match f {
        [only] => parse_format(Some(only)).unwrap_or_default(),
        _ => SubsonicFormat::Xml,
    };
    let callback = match callback {
        [only] if only.len() <= 128 => Some((*only).to_owned()),
        _ => None,
    };
    (format, callback)
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
    /// A streamed body, sent instead of `body` (audio only).
    pub stream: Option<crate::body::LiveBody>,
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

    fn text(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: content_type.to_owned(),
            headers: Vec::new(),
            body,
            stream: None,
        }
    }
}

/// A handler's answer, serialized once as JSON text in wire order.
pub type Payload = Box<RawValue>;

/// Serialize a model as a payload.
pub fn payload<T: Serialize>(value: &T) -> Result<Payload, SubsonicError> {
    let text = to_wire_json(value).map_err(serialize_failed)?;
    RawValue::from_string(text).map_err(serialize_failed)
}

fn serialize_failed(err: serde_json::Error) -> SubsonicError {
    SubsonicError::server_fault(ServerFault::Internal(format!(
        "subsonic answer did not serialize: {err}"
    )))
}

/// The writer's formatter: compact JSON, with floats and two control
/// escapes as v2 wrote them.
struct Wire;

impl Formatter for Wire {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        writer.write_all(format_float(value).as_bytes())
    }

    fn write_char_escape<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        char_escape: CharEscape,
    ) -> io::Result<()> {
        match char_escape {
            CharEscape::Backspace => writer.write_all(b"\\u0008"),
            CharEscape::FormFeed => writer.write_all(b"\\u000c"),
            other => CompactFormatter.write_char_escape(writer, other),
        }
    }
}

fn to_wire_json<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    let mut out = Vec::new();
    value.serialize(&mut serde_json::Serializer::with_formatter(&mut out, Wire))?;
    String::from_utf8(out).map_err(|err| serde::ser::Error::custom(err.to_string()))
}

/// A float as v2 wrote it: a whole value below 1e15 keeps one decimal
/// place, anything else is the shortest form that reads back the same.
fn format_float(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// The error body of a failed envelope.
#[derive(serde::Serialize)]
struct SubsonicFailure<'a> {
    code: u8,
    message: &'a str,
}

/// The envelope (v2 `_envelope`): five attributes, then the answer under
/// its key.
struct Envelope<'a> {
    status: &'a str,
    server_name: &'a str,
    server_version: &'a str,
    body: Option<(&'a str, &'a RawValue)>,
}

impl Serialize for Envelope<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("status", self.status)?;
        map.serialize_entry("version", API_VERSION)?;
        map.serialize_entry("type", self.server_name)?;
        map.serialize_entry("serverVersion", self.server_version)?;
        map.serialize_entry("openSubsonic", &true)?;
        if let Some((key, value)) = self.body {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// The JSON document: the envelope under `subsonic-response`.
#[derive(serde::Serialize)]
struct Document<'a> {
    #[serde(rename = "subsonic-response")]
    response: &'a Envelope<'a>,
}

/// Emit the envelope in the requested format (v2 `_emit`).
fn emit(
    envelope: &Envelope<'_>,
    format: SubsonicFormat,
    callback: Option<&str>,
) -> Result<Rendered, serde_json::Error> {
    if format == SubsonicFormat::Xml {
        let xml = to_xml(&to_wire_json(envelope)?)?;
        return Ok(Rendered::text(200, "application/xml", xml.into_bytes()));
    }
    let json = to_wire_json(&Document { response: envelope })?;
    let safe_callback = callback.filter(|cb| callback_is_safe(cb));
    Ok(match (format, safe_callback) {
        (SubsonicFormat::Jsonp, Some(cb)) => Rendered::text(
            200,
            "application/javascript",
            format!("{cb}({json});").into_bytes(),
        ),
        _ => Rendered::text(200, "application/json", json.into_bytes()),
    })
}

/// Render an `ok` envelope (v2 `render`).
pub fn render_ok(
    body: Option<(&str, &RawValue)>,
    format: SubsonicFormat,
    callback: Option<&str>,
    server_name: &str,
    server_version: &str,
) -> Result<Rendered, SubsonicError> {
    let envelope = Envelope {
        status: "ok",
        server_name,
        server_version,
        body,
    };
    emit(&envelope, format, callback).map_err(serialize_failed)
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
    let rendered = payload(&SubsonicFailure { code, message }).and_then(|error| {
        let envelope = Envelope {
            status: "failed",
            server_name,
            server_version,
            body: Some(("error", &error)),
        };
        emit(&envelope, format, callback).map_err(serialize_failed)
    });
    rendered.unwrap_or_else(|err| {
        tracing::error!(error = %err, "subsonic failure envelope did not render");
        Rendered::text(500, "text/plain", Vec::new())
    })
}

/// Render a binary-path failure as `text/plain`: 70 maps to 404, 50 to
/// 403, anything else to 404 (v2 `_binary_error`).
pub fn render_binary_error(code: u8, message: &str) -> Rendered {
    let status = match code {
        NOT_FOUND => 404,
        NOT_AUTHORIZED => 403,
        _ => 404,
    };
    Rendered::text(status, "text/plain", message.as_bytes().to_vec())
}

/// The answer to a method that does not exist: a plain 404 in every
/// format, the body Navidrome 0.62.0 sends (recording `error.unknown-method`).
pub fn render_unknown_method() -> Rendered {
    Rendered::text(404, "text/plain", b"404 page not found\n".to_vec())
}

/// Write an envelope's JSON text as the XML document (v2 `_to_xml`).
fn to_xml(envelope_json: &str) -> Result<String, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(envelope_json);
    let root = Node::deserialize(&mut deserializer)?;
    deserializer.end()?;
    let mut element = match root {
        Node::Element(element) => element,
        _ => Element::default(),
    };
    element.attrs = format!(" xmlns=\"{NAMESPACE}\"{}", element.attrs);
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    element.write("subsonic-response", &mut out);
    Ok(out)
}

/// One JSON value on its way to XML.
enum Node {
    Null,
    Scalar(String),
    Element(Element),
    List(Vec<Node>),
}

/// An object's attributes, text and children, written but not yet named.
#[derive(Default)]
struct Element {
    attrs: String,
    text: String,
    children: String,
}

impl Element {
    fn add(&mut self, key: &str, node: Node) {
        match node {
            Node::Null => {}
            node if key == "value" => {
                self.text = match node {
                    Node::Scalar(text) => xml_text(&text),
                    _ => String::new(),
                };
            }
            Node::Scalar(text) => {
                self.attrs.push(' ');
                self.attrs.push_str(key);
                self.attrs.push_str("=\"");
                self.attrs.push_str(&xml_attr(&text));
                self.attrs.push('"');
            }
            node => write_node(key, node, &mut self.children),
        }
    }

    fn write(self, tag: &str, out: &mut String) {
        out.push('<');
        out.push_str(tag);
        out.push_str(&self.attrs);
        if self.text.is_empty() && self.children.is_empty() {
            out.push_str("/>");
        } else {
            out.push('>');
            out.push_str(&self.text);
            out.push_str(&self.children);
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
    }
}

/// A child element, or one per item of a list, named `tag`.
fn write_node(tag: &str, node: Node, out: &mut String) {
    match node {
        Node::Null => {}
        Node::Scalar(text) => {
            out.push('<');
            out.push_str(tag);
            out.push('>');
            out.push_str(&xml_text(&text));
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        Node::Element(element) => element.write(tag, out),
        Node::List(items) => {
            for item in items {
                write_node(tag, item, out);
            }
        }
    }
}

/// Text content: C0 controls illegal in XML 1.0 dropped, then escaped.
fn xml_text(raw: &str) -> String {
    quick_xml::escape::partial_escape(xml_chars(raw)).into_owned()
}

/// An attribute value: C0 controls illegal in XML 1.0 dropped, then
/// escaped with quotes.
fn xml_attr(raw: &str) -> String {
    quick_xml::escape::escape(xml_chars(raw)).into_owned()
}

/// Drop the C0 controls XML 1.0 forbids (tab, newline and CR stay).
fn xml_chars(raw: &str) -> Cow<'_, str> {
    let illegal = |c: char| matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}');
    if raw.contains(illegal) {
        Cow::Owned(raw.chars().filter(|c| !illegal(*c)).collect())
    } else {
        Cow::Borrowed(raw)
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(NodeVisitor)
    }
}

struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Subsonic answer")
    }

    fn visit_unit<E>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Node, E> {
        Ok(Node::Scalar(value.to_string()))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Node, E> {
        Ok(Node::Scalar(value.to_string()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Node, E> {
        Ok(Node::Scalar(value.to_string()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Node, E> {
        Ok(Node::Scalar(format_float(value)))
    }

    fn visit_str<E>(self, value: &str) -> Result<Node, E> {
        Ok(Node::Scalar(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Node, E> {
        Ok(Node::Scalar(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element::<Node>()? {
            items.push(item);
        }
        Ok(Node::List(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let mut element = Element::default();
        while let Some((key, node)) = map.next_entry::<Cow<'de, str>, Node>()? {
            element.add(&key, node);
        }
        Ok(Node::Element(element))
    }
}
