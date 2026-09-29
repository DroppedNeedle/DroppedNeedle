//! A minimal XML reader for the Newznab feed shapes.
//!
//! Newznab is XML-only (v2 `newznab_client.py`: neither Lidarr nor Prowlarr
//! ever sends `o=json`), but the crate carries no XML dependency and this
//! slice adds none, so the reader here covers exactly what the responses
//! need: elements, attributes, text, comments, processing instructions,
//! CDATA, and entity decoding. Failures surface as typed errors, never
//! panics, and the v2 hardening (illegal-character strip, bare-ampersand
//! escape a la Prowlarr's `XmlCleaner`) runs before parsing.

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
    pub offset: usize,
    /// Human-readable cause.
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "XML at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Strip the XML 1.0 illegal characters v2's `_ILLEGAL_XML` covers (C0
/// controls except tab/newline/CR, the C1 range, U+FFFE/U+FFFF).
pub fn strip_illegal(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            let value = u32::from(*ch);
            !matches!(value, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F..=0x9F | 0xFFFE | 0xFFFF)
        })
        .collect()
}

/// Escape bare `&` and non-predefined `&name;` entities (v2 `_BARE_AMP`).
pub fn escape_bare_amps(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'&' {
            out.push(bytes[index] as char);
            index += 1;
            continue;
        }
        let rest = &text[index..];
        if is_predefined_entity(rest) {
            out.push('&');
            index += 1;
            continue;
        }
        out.push_str("&amp;");
        index += 1;
    }
    out
}

fn is_predefined_entity(rest: &str) -> bool {
    for entity in ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"] {
        if rest.starts_with(entity) {
            return true;
        }
    }
    if let Some(end) = rest.find(';') {
        let body = &rest[1..end];
        if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
            return !hex.is_empty()
                && hex.len() <= 6
                && hex.chars().all(|ch| ch.is_ascii_hexdigit());
        }
        if let Some(decimal) = body.strip_prefix('#') {
            return !decimal.is_empty()
                && decimal.len() <= 7
                && decimal.chars().all(|ch| ch.is_ascii_digit());
        }
    }
    false
}

/// Decode the predefined + numeric entities in character data.
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            break;
        };
        let entity = &rest[..=end];
        if let Some(decoded) = decode_entity(entity) {
            out.push_str(&decoded);
        } else {
            out.push_str(entity);
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<String> {
    match entity {
        "&amp;" => Some("&".to_owned()),
        "&lt;" => Some("<".to_owned()),
        "&gt;" => Some(">".to_owned()),
        "&quot;" => Some("\"".to_owned()),
        "&apos;" => Some("'".to_owned()),
        _ => {
            let body = entity.strip_prefix("&#")?.strip_suffix(';')?;
            let value = if let Some(hex) = body.strip_prefix('x').or_else(|| body.strip_prefix('X'))
            {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                body.parse::<u32>().ok()?
            };
            char::from_u32(value).map(|ch| ch.to_string())
        }
    }
}

/// Parse one document; the XML declaration, comments, processing
/// instructions, and DOCTYPE are skipped, CDATA lands in the text.
pub fn parse(text: &str) -> Result<Element, ParseError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        index: 0,
    };
    parser.parse_document()
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    index: usize,
}

impl<'a> Parser<'a> {
    fn parse_document(&mut self) -> Result<Element, ParseError> {
        loop {
            self.skip_space();
            if self.at_end() {
                return self.fail("empty document");
            }
            if self.consume("<!--") {
                self.skip_until("-->")?;
                continue;
            }
            if self.consume("<?") {
                self.skip_until("?>")?;
                continue;
            }
            if self.consume("<!DOCTYPE") || self.consume("<!doctype") {
                self.skip_until(">")?;
                continue;
            }
            if self.peek() == Some(b'<') {
                break;
            }
            return self.fail("expected an element");
        }
        self.parse_element()
    }

    fn parse_element(&mut self) -> Result<Element, ParseError> {
        if !self.consume("<") {
            return self.fail("expected '<'");
        }
        let tag = self.parse_name()?;
        let mut attrs = Vec::new();
        loop {
            self.skip_space();
            if self.at_end() {
                return self.fail("unterminated start tag");
            }
            if self.consume("/>") {
                return Ok(Element {
                    tag,
                    attrs,
                    children: Vec::new(),
                    text: String::new(),
                });
            }
            if self.consume(">") {
                break;
            }
            let name = self.parse_name()?;
            self.skip_space();
            if !self.consume("=") {
                return self.fail("expected '=' after attribute name");
            }
            self.skip_space();
            let value = self.parse_attr_value()?;
            attrs.push((name, value));
        }
        let mut children = Vec::new();
        let mut text = String::new();
        loop {
            if self.at_end() {
                return self.fail("unterminated element");
            }
            if self.bytes[self.index] == b'<' {
                if self.consume("</") {
                    let closing = self.parse_name()?;
                    if closing != tag {
                        return self.fail(format!("mismatched close tag '{closing}'"));
                    }
                    self.skip_space();
                    if !self.consume(">") {
                        return self.fail("expected '>' after close tag");
                    }
                    break;
                }
                if self.consume("<!--") {
                    self.skip_until("-->")?;
                    continue;
                }
                if self.consume("<?") {
                    self.skip_until("?>")?;
                    continue;
                }
                if self.consume("<![CDATA[") {
                    text.push_str(&self.read_until("]]>")?);
                    continue;
                }
                children.push(self.parse_element()?);
                continue;
            }
            text.push_str(&self.read_text());
        }
        Ok(Element {
            tag,
            attrs,
            children,
            text: decode_entities(text.trim()).trim().to_owned(),
        })
    }

    fn parse_name(&mut self) -> Result<String, ParseError> {
        let start = self.index;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>' | b'=' | b'<' | b'?' | b'!')
            {
                break;
            }
            self.index += 1;
        }
        if start == self.index {
            return self.fail("expected a name");
        }
        self.text
            .get(start..self.index)
            .map(str::to_owned)
            .ok_or_else(|| self.error_at(start, "name is not valid UTF-8"))
    }

    fn parse_attr_value(&mut self) -> Result<String, ParseError> {
        let quote = match self.peek() {
            Some(b'"') => b'"',
            Some(b'\'') => b'\'',
            _ => return self.fail("expected a quoted attribute value"),
        };
        self.index += 1;
        let start = self.index;
        while let Some(byte) = self.peek() {
            if byte == quote {
                let raw = self
                    .text
                    .get(start..self.index)
                    .ok_or_else(|| self.error_at(start, "attribute is not valid UTF-8"))?;
                self.index += 1;
                return Ok(decode_entities(raw));
            }
            self.index += 1;
        }
        self.fail("unterminated attribute value")
    }

    fn read_text(&mut self) -> String {
        let start = self.index;
        while let Some(byte) = self.peek() {
            if byte == b'<' {
                break;
            }
            self.index += 1;
        }
        self.text.get(start..self.index).unwrap_or("").to_owned()
    }

    fn read_until(&mut self, end: &str) -> Result<String, ParseError> {
        let start = self.index;
        if let Some(offset) = self.text[start..].find(end) {
            let out = self.text[start..start + offset].to_owned();
            self.index = start + offset + end.len();
            Ok(out)
        } else {
            self.fail(format!("unterminated section, expected '{end}'"))
        }
    }

    fn skip_until(&mut self, end: &str) -> Result<(), ParseError> {
        self.read_until(end).map(|_| ())
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(byte) if byte.is_ascii_whitespace()) {
            self.index += 1;
        }
    }

    fn consume(&mut self, token: &str) -> bool {
        if self.text[self.index..].starts_with(token) {
            self.index += token.len();
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn at_end(&self) -> bool {
        self.index >= self.bytes.len()
    }

    fn fail<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(self.error_at(self.index, message))
    }

    fn error_at(&self, offset: usize, message: impl Into<String>) -> ParseError {
        ParseError {
            offset,
            message: message.into(),
        }
    }
}
