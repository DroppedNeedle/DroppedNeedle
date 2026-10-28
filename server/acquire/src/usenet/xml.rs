//! Reads a Newznab response into a small element tree.
//!
//! Newznab is XML-only (neither Lidarr nor Prowlarr ever sends
//! `o=json`). quick-xml does the tokenising. It fetches nothing,
//! hands a DOCTYPE over as one opaque event and knows only the five
//! predefined entities and numeric character references, so a document
//! cannot grow by entity expansion. The tree is built here with a stack
//! instead of recursion, and its depth and size are bounded, so a hostile
//! indexer gets an error back and nothing else.
//!
//! Real feeds are not always well formed. [`strip_illegal`] and
//! [`escape_bare_amps`] carry the hardening (illegal-character strip,
//! bare-ampersand escape a la Prowlarr's `XmlCleaner`) that the caller
//! applies around [`parse`].

use quick_xml::Reader;
use quick_xml::escape::{resolve_predefined_entity, unescape};
use quick_xml::events::{BytesStart, Event};

/// Deepest nesting accepted. A feed is `rss > channel > item > attr` and
/// caps is `caps > categories > category > subcat`: four levels.
pub const MAX_DEPTH: usize = 64;

/// Most elements and attributes accepted in one document. Each
/// `newznab:attr` is three nodes (the element, `name`, `value`), so an
/// item with 25 of them plus its own dozen elements and attributes is
/// about 90 to 150 nodes, and the largest page an indexer serves, 1000
/// items, is 90,000 to 150,000: this leaves that page 2.5 times over. The
/// bound keeps a body made of nothing but tiny tags or empty attributes
/// from costing many times its size in memory (about 40 MB at the bound).
/// The per-item figure is read off the recorded feeds, not measured on a
/// live 1000-item page.
pub const MAX_NODES: usize = 400_000;

/// One element: tag, attributes, child elements, and direct text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// Full tag as written (`newznab:attr` keeps its prefix; use [`local`]).
    pub tag: String,
    /// Attributes in document order.
    pub attrs: Vec<(String, String)>,
    /// Child elements.
    pub children: Vec<Element>,
    /// Direct character data, entity-decoded and trimmed.
    pub text: String,
}

impl Element {
    /// Attribute value by exact name.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// First child whose local tag matches `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| local(&child.tag) == name)
    }

    /// All children whose local tag matches `name`.
    pub fn children_named(&self, name: &str) -> impl Iterator<Item = &Element> {
        self.children
            .iter()
            .filter(move |child| local(&child.tag) == name)
    }

    /// Every descendant (depth-first) whose local tag matches `name`.
    pub fn descendants(&self, name: &str) -> Vec<&Element> {
        let mut out = Vec::new();
        collect_descendants(self, name, &mut out);
        out
    }

    /// Text of the first child whose local tag matches `name`.
    pub fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name).map(|child| child.text.as_str())
    }
}

/// Recursion here is at most [`MAX_DEPTH`] deep: [`parse`] builds no
/// deeper tree.
fn collect_descendants<'a>(element: &'a Element, name: &str, out: &mut Vec<&'a Element>) {
    for child in &element.children {
        if local(&child.tag) == name {
            out.push(child);
        }
        collect_descendants(child, name, out);
    }
}

/// Strip any namespace prefix (`newznab:attr` -> `attr`, `{ns}attr` -> `attr`).
pub fn local(tag: &str) -> &str {
    let tag = tag.rsplit('}').next().unwrap_or(tag);
    tag.rsplit(':').next().unwrap_or(tag)
}

/// Parse failure: byte offset plus what went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Byte offset where parsing stopped.
    pub offset: u64,
    /// Human-readable cause.
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "XML at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Strip the XML 1.0 illegal characters (C0
/// controls except tab/newline/CR, the C1 range, U+FFFE/U+FFFF).
pub fn strip_illegal(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            let value = u32::from(*ch);
            !matches!(value, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F..=0x9F | 0xFFFE | 0xFFFF)
        })
        .collect()
}

/// Escape every `&` that does not start a reference the parser resolves:
/// a bare one, `&nbsp;` and other undeclared names. They
/// come out as literal text. CDATA sections are left as they are.
pub fn escape_bare_amps(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<![CDATA[") {
        let end = rest[start..]
            .find("]]>")
            .map_or(rest.len(), |at| start + at + "]]>".len());
        escape_amps_into(&rest[..start], &mut out);
        out.push_str(&rest[start..end]);
        rest = &rest[end..];
    }
    escape_amps_into(rest, &mut out);
    out
}

/// The longest reference the parser resolves, `&#x10FFFF;`, is 10 bytes.
/// Looking no further for the `;` keeps the escape pass linear: a body of
/// nothing but ampersands would otherwise be rescanned to its end from
/// every one of them.
const REFERENCE_WINDOW: usize = 12;

fn escape_amps_into(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let resolves = rest
            .bytes()
            .take(REFERENCE_WINDOW)
            .position(|byte| byte == b';')
            .is_some_and(|end| unescape(&rest[..=end]).is_ok());
        out.push_str(if resolves { "&" } else { "&amp;" });
        rest = &rest[1..];
    }
    out.push_str(rest);
}

/// Parse one document into its root element. The XML declaration,
/// comments, processing instructions and any DOCTYPE are skipped, CDATA
/// lands in the text, and anything after the root is ignored.
pub fn parse(text: &str) -> Result<Element, ParseError> {
    let mut reader = Reader::from_str(text);
    let mut open: Vec<Element> = Vec::new();
    let mut nodes = 0_usize;
    loop {
        let event = reader.read_event().map_err(|error| ParseError {
            offset: reader.error_position(),
            message: error.to_string(),
        })?;
        let fail = |message: &str| ParseError {
            offset: reader.buffer_position(),
            message: message.to_owned(),
        };
        let closed = match event {
            Event::Start(start) => {
                let element = element_from(&start, open.len(), &mut nodes)
                    .map_err(|message| fail(&message))?;
                open.push(element);
                continue;
            }
            Event::Empty(start) => {
                element_from(&start, open.len(), &mut nodes).map_err(|message| fail(&message))?
            }
            Event::End(_) => {
                let mut element = open.pop().ok_or_else(|| fail("unexpected close tag"))?;
                element.text = element.text.trim().to_owned();
                element
            }
            Event::Text(chunk) => {
                match open.last_mut() {
                    Some(element) => element.text.push_str(&chunk),
                    None if chunk.trim().is_empty() => {}
                    None => return Err(fail("expected an element")),
                }
                continue;
            }
            Event::CData(chunk) => {
                let element = open.last_mut().ok_or_else(|| fail("expected an element"))?;
                element.text.push_str(&chunk);
                continue;
            }
            Event::GeneralRef(reference) => {
                let element = open.last_mut().ok_or_else(|| fail("expected an element"))?;
                match reference.resolve_char_ref() {
                    Ok(Some(ch)) => element.text.push(ch),
                    Ok(None) => match resolve_predefined_entity(&reference) {
                        Some(value) => element.text.push_str(value),
                        None => return Err(fail("unknown entity")),
                    },
                    Err(_) => return Err(fail("invalid character reference")),
                }
                continue;
            }
            Event::Eof if open.is_empty() => return Err(fail("empty document")),
            Event::Eof => return Err(fail("unterminated element")),
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => continue,
        };
        match open.last_mut() {
            Some(parent) => parent.children.push(closed),
            None => return Ok(closed),
        }
    }
}

/// A start tag as an element with no children yet, refused when it would
/// take the tree past its bounds. Repeated attribute names are kept, as
/// feeds in the wild are not always tidy.
fn element_from(
    start: &BytesStart<'_>,
    depth: usize,
    nodes: &mut usize,
) -> Result<Element, String> {
    if depth >= MAX_DEPTH {
        return Err("nested too deeply".to_owned());
    }
    *nodes += 1;
    if *nodes > MAX_NODES {
        return Err("too many elements".to_owned());
    }
    let mut attributes = start.attributes();
    attributes.with_checks(false);
    let mut attrs = Vec::new();
    for attribute in attributes {
        let attribute = attribute.map_err(|error| error.to_string())?;
        *nodes += 1;
        if *nodes > MAX_NODES {
            return Err("too many attributes".to_owned());
        }
        let value = unescape(&attribute.value).map_err(|error| error.to_string())?;
        attrs.push((attribute.key.0.to_owned(), value.into_owned()));
    }
    Ok(Element {
        tag: start.name().0.to_owned(),
        attrs,
        children: Vec::new(),
        text: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retry path must not damage what was already fine: text outside
    /// ASCII, real references, and CDATA, which is literal by definition.
    #[test]
    fn escaping_bare_ampersands_leaves_the_rest_alone() {
        let raw = "<t a=\"x?a=1&b=2\">Beyoncé & Jay-Z &amp; more&nbsp;&#233;<![CDATA[R&B]]></t>";
        assert!(parse(raw).is_err(), "not well formed as sent");
        let root = parse(&escape_bare_amps(raw)).expect("parses once escaped");
        assert_eq!(root.attr("a"), Some("x?a=1&b=2"));
        assert_eq!(root.text, "Beyoncé & Jay-Z & more&nbsp;éR&B");
    }

    /// Attributes cost memory like elements do, so one tag cannot carry
    /// more of them than a whole document may hold elements.
    #[test]
    fn attributes_count_toward_the_size_bound() {
        let within = format!("<t{}/>", " a=\"\"".repeat(1000));
        assert_eq!(
            parse(&within).expect("a wide tag is fine").attrs.len(),
            1000
        );
        let beyond = format!("<t{}/>", " a=\"\"".repeat(MAX_NODES));
        let error = parse(&beyond).expect_err("too many attributes");
        assert!(error.message.contains("too many"), "{error}");
    }

    /// The escape pass looks only a few bytes past each `&` for its `;`,
    /// so its work grows with the input and not with its square. A `;`
    /// further off than any real reference is not found at all, and a
    /// megabyte of ampersands comes out escaped one by one.
    #[test]
    fn escaping_looks_a_bounded_distance_for_the_semicolon() {
        let far = format!("&{};", "x".repeat(REFERENCE_WINDOW));
        assert_eq!(escape_bare_amps(&far), format!("&amp;{}", &far[1..]));
        assert_eq!(escape_bare_amps("&#x10FFFF;"), "&#x10FFFF;");

        let flood = "&".repeat(1024 * 1024);
        assert_eq!(escape_bare_amps(&flood), "&amp;".repeat(1024 * 1024));
    }
}
