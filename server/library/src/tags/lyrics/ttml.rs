//! TTML lyrics (`.ttml` files, Apple's word-timed format), read through
//! quick-xml into a small bounded tree, then into lines.
//!
//! The document is untrusted. quick-xml fetches nothing; a DOCTYPE is
//! refused, only the predefined and numeric entities are known, nesting
//! stops at [`MAX_DEPTH`], elements and attributes at [`MAX_NODES`], and
//! the text, read and built, at the `.lrc` caps. Past [`MAX_AGENTS`]
//! singers, [`MAX_LAYER_CANDIDATES`] translation or pronunciation
//! languages read and [`MAX_LAYERS`] kept, the rest are left out. A head
//! text feeds only the first line naming its key. A document that breaks
//! a rule reads as no lyrics.
//!
//! Reading:
//! - `tt@itunes:timing`: `Word` or none gives word cues from timed spans,
//!   `Line` line times only, `None` no times. `tt@xml:lang` is the main
//!   language.
//! - Head: singers (`ttm:agent` with `type`, `xml:id`, a `ttm:name`; the
//!   first of a repeated id wins), and `translations` and
//!   `transliterations` whose `text[for]` name a line by its `itunes:key`.
//! - Body `p`: `begin`, `end` or `dur`, `itunes:key`, one `ttm:agent` (its
//!   own, else its `div`'s). A timed `span` is a piece; other text is plain;
//!   `br` is a space. A `span` with `ttm:role="x-bg"` holds background
//!   vocals, a cue line of their own on the same line; `x-translation` and
//!   `x-roman` spans are layer text, by `xml:lang` (`und` without one).
//! - Times: clock (`h:mm:ss.f`, `m:ss.f`) and bare seconds are absolute;
//!   a number with a unit (`h`, `m`, `s`, `ms`) counts from the nearest
//!   timed ancestor's begin. Fractions are read as digits, never floats.
//!   `f` and `t` units and times past 100 hours leave the element untimed.
//! - Whitespace runs collapse to one space (`xml:space` is not read).
//! - Lines are sorted by start (stably), so line indexes follow time.
//! - Singers: the main one is the singer of the earliest line with word
//!   cues (not a group), else the first non-group singer; a line naming
//!   none is the main one's. Background vocals get a singer of their own
//!   (`bg`), and a made-up main one when none is named; made-up ids never
//!   take a declared one.

use std::collections::HashMap;

use quick_xml::Reader;
use quick_xml::escape::{resolve_predefined_entity, unescape};
use quick_xml::events::{BytesStart, Event};

use super::line::{
    Budget, MAX_TIME_MS, Piece, RawLine, RawPart, RawTrack, build_track, free_agent_id,
};
use super::{
    Agent, AgentRole, Lyrics, LyricsKind, MAX_LYRICS_BYTES, MAX_LYRICS_CHARACTERS, valid_lang,
};

/// Deepest nesting read.
const MAX_DEPTH: usize = 32;
/// Most elements and attributes read.
const MAX_NODES: usize = 100_000;
/// Most singers read.
const MAX_AGENTS: usize = 64;
/// Most translation and pronunciation tracks kept, of those that end up
/// with lines.
const MAX_LAYERS: usize = 8;
/// Most translation and pronunciation languages read, of those holding
/// some text: past it a new language is left out. Some may match no line,
/// so more are read than kept.
const MAX_LAYER_CANDIDATES: usize = 64;
/// Most digits read in one run of a time.
const MAX_TIME_DIGITS: usize = 9;

/// The lyrics of a TTML document. `Ok(None)` when it holds no line of
/// text; `Err` names why it was refused.
pub(super) fn parse_ttml(text: &str) -> Result<Option<Lyrics>, &'static str> {
    if text.len() > MAX_LYRICS_BYTES {
        return Err("over the byte cap");
    }
    if text.chars().count() > MAX_LYRICS_CHARACTERS {
        return Err("over the character cap");
    }
    let root = read_tree(text)?;
    if root.name != "tt" {
        return Err("not a TTML document");
    }
    let timing = match root.attr("timing") {
        Some(value) if value.eq_ignore_ascii_case("line") => Timing::Line,
        Some(value) if value.eq_ignore_ascii_case("none") => Timing::None,
        _ => Timing::Word,
    };
    let lang = root.attr("lang").and_then(valid_lang);
    let mut layers = Layers::default();
    let mut head = Head::default();
    if let Some(element) = root.child("head") {
        head.read(element, &mut layers);
    }
    let mut paragraphs = Vec::new();
    if let Some(body) = root.child("body") {
        let base = begin_of(body, 0).unwrap_or(0);
        blocks(body, None, base, &mut layers, &mut paragraphs);
    }
    assemble(timing, lang, head, &layers, paragraphs)
}

/// The translation and pronunciation languages met, in order; an index
/// into it names a layer.
#[derive(Default)]
struct Layers {
    known: Vec<(LyricsKind, String)>,
}

impl Layers {
    /// The layer of `kind` and `lang`, taken on when there is room.
    fn index(&mut self, kind: LyricsKind, lang: &str) -> Option<usize> {
        if let Some(index) = self
            .known
            .iter()
            .position(|(known_kind, known_lang)| *known_kind == kind && known_lang == lang)
        {
            return Some(index);
        }
        if self.known.len() >= MAX_LAYER_CANDIDATES {
            return None;
        }
        self.known.push((kind, lang.to_owned()));
        Some(self.known.len() - 1)
    }
}

/// What `itunes:timing` says the document times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Timing {
    Word,
    Line,
    None,
}

/// One element: its local name, attributes by local name, and children.
#[derive(Debug, Default)]
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

#[derive(Debug)]
enum Node {
    Element(Element),
    Text(String),
}

impl Element {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            Node::Text(_) => None,
        })
    }

    fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|element| element.name == name)
    }

    fn push_text(&mut self, text: &str) {
        match self.children.last_mut() {
            Some(Node::Text(last)) => last.push_str(text),
            _ => self.children.push(Node::Text(text.to_owned())),
        }
    }
}

/// Read the document into its root element, within the bounds.
fn read_tree(text: &str) -> Result<Element, &'static str> {
    let mut reader = Reader::from_str(text);
    let mut open: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut nodes = 0_usize;
    loop {
        let event = reader.read_event().map_err(|_| "not well-formed XML")?;
        let closed = match event {
            Event::DocType(_) => return Err("a DOCTYPE is refused"),
            Event::Start(start) => {
                if root.is_some() {
                    return Err("more than one root");
                }
                if open.len() >= MAX_DEPTH {
                    return Err("nested too deeply");
                }
                open.push(element_from(&start, &mut nodes)?);
                continue;
            }
            Event::Empty(start) => {
                if root.is_some() {
                    return Err("more than one root");
                }
                if open.len() >= MAX_DEPTH {
                    return Err("nested too deeply");
                }
                element_from(&start, &mut nodes)?
            }
            Event::End(_) => open.pop().ok_or("unexpected close tag")?,
            Event::Text(chunk) => {
                match open.last_mut() {
                    Some(element) => element.push_text(&chunk),
                    None if chunk.trim().is_empty() => {}
                    None => return Err("text outside the root"),
                }
                continue;
            }
            Event::CData(chunk) => {
                open.last_mut()
                    .ok_or("text outside the root")?
                    .push_text(&chunk);
                continue;
            }
            Event::GeneralRef(reference) => {
                let element = open.last_mut().ok_or("text outside the root")?;
                match reference.resolve_char_ref() {
                    Ok(Some(ch)) => element.push_text(ch.encode_utf8(&mut [0; 4])),
                    Ok(None) => match resolve_predefined_entity(&reference) {
                        Some(value) => element.push_text(value),
                        None => return Err("unknown entity"),
                    },
                    Err(_) => return Err("invalid character reference"),
                }
                continue;
            }
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) => continue,
            Event::Eof => break,
        };
        match open.last_mut() {
            Some(parent) => parent.children.push(Node::Element(closed)),
            None => root = Some(closed),
        }
    }
    if !open.is_empty() {
        return Err("unterminated element");
    }
    root.ok_or("empty document")
}

/// A start tag as an element with no children yet, counted against the
/// node bound.
fn element_from(start: &BytesStart<'_>, nodes: &mut usize) -> Result<Element, &'static str> {
    *nodes += 1;
    if *nodes > MAX_NODES {
        return Err("too many elements");
    }
    let mut attrs = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|_| "malformed attribute")?;
        *nodes += 1;
        if *nodes > MAX_NODES {
            return Err("too many attributes");
        }
        let value = unescape(&attribute.value).map_err(|_| "unknown entity")?;
        attrs.push((
            attribute.key.local_name().as_ref().to_owned(),
            value.into_owned(),
        ));
    }
    Ok(Element {
        name: start.local_name().as_ref().to_owned(),
        attrs,
        children: Vec::new(),
    })
}

/// A declared singer.
struct Declared {
    id: String,
    group: bool,
    name: Option<String>,
}

/// What the head holds.
#[derive(Default)]
struct Head {
    agents: Vec<Declared>,
    /// Layer text by line key, by layer index.
    texts: Vec<HashMap<String, String>>,
}

impl Head {
    fn read(&mut self, element: &Element, layers: &mut Layers) {
        for child in element.elements() {
            match child.name.as_str() {
                "agent" => self.agent(child),
                "translation" => self.layer(child, LyricsKind::Translation, layers),
                "transliteration" => self.layer(child, LyricsKind::Pronunciation, layers),
                _ => self.read(child, layers),
            }
        }
    }

    fn agent(&mut self, element: &Element) {
        let Some(id) = element.attr("id") else {
            return;
        };
        if self.agents.len() >= MAX_AGENTS || self.agents.iter().any(|agent| agent.id == id) {
            return;
        }
        let name = element
            .child("name")
            .map(|name| collapse(&text_of(name)).trim().to_owned())
            .filter(|name| !name.is_empty());
        self.agents.push(Declared {
            id: id.to_owned(),
            group: element
                .attr("type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("group")),
            name,
        });
    }

    /// One language's texts, kept when it holds some and there is room.
    fn layer(&mut self, element: &Element, kind: LyricsKind, layers: &mut Layers) {
        let mut texts = HashMap::new();
        for text in element.elements().filter(|child| child.name == "text") {
            if let Some(key) = text.attr("for") {
                let value = collapse(&text_of(text));
                if !value.trim().is_empty() {
                    texts.entry(key.to_owned()).or_insert(value);
                }
            }
        }
        if texts.is_empty() {
            return;
        }
        let Some(index) = layers.index(kind, &layer_lang(element)) else {
            return;
        };
        if self.texts.len() <= index {
            self.texts.resize_with(index + 1, HashMap::new);
        }
        for (key, text) in texts {
            self.texts[index].entry(key).or_insert(text);
        }
    }
}

/// A layer's language: its `xml:lang` when valid, else `und`.
fn layer_lang(element: &Element) -> String {
    element
        .attr("lang")
        .and_then(valid_lang)
        .unwrap_or_else(|| "und".to_owned())
}

/// Every text below `element`, `br` as a space.
fn text_of(element: &Element) -> String {
    let mut out = String::new();
    for node in &element.children {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Element(child) if child.name == "br" => out.push(' '),
            Node::Element(child) => out.push_str(&text_of(child)),
        }
    }
    out
}

/// Every XML whitespace run as one space.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r') {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

/// One `p`, read.
struct Paragraph {
    start: Option<i64>,
    end: Option<i64>,
    agent: Option<String>,
    key: Option<String>,
    main: Vec<Piece>,
    background: Vec<Piece>,
    /// Inline layer text by layer index.
    layers: Vec<(usize, String)>,
}

/// Walk `body` and its `div`s for `p`s, carrying the agent and the time
/// base down.
fn blocks(
    element: &Element,
    agent: Option<&str>,
    base: i64,
    layers: &mut Layers,
    out: &mut Vec<Paragraph>,
) {
    for child in element.elements() {
        match child.name.as_str() {
            "div" => {
                let agent = child.attr("agent").or(agent);
                let base = begin_of(child, base).unwrap_or(base);
                blocks(child, agent, base, layers, out);
            }
            "p" => out.push(paragraph(child, agent, base, layers)),
            _ => {}
        }
    }
}

fn paragraph(element: &Element, agent: Option<&str>, base: i64, layers: &mut Layers) -> Paragraph {
    let begin = begin_of(element, base);
    let end = end_of(element, base, begin);
    let mut paragraph = Paragraph {
        start: begin,
        end,
        agent: element.attr("agent").or(agent).map(str::to_owned),
        key: element.attr("key").map(str::to_owned),
        main: Vec::new(),
        background: Vec::new(),
        layers: Vec::new(),
    };
    let mut into = Sink {
        paragraph: &mut paragraph,
        layers,
    };
    collect(
        &element.children,
        &Role::Main,
        begin.unwrap_or(base),
        &mut into,
    );
    paragraph
}

/// Where a paragraph's text goes.
struct Sink<'a> {
    paragraph: &'a mut Paragraph,
    layers: &'a mut Layers,
}

/// Which part of a line text belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Role {
    Main,
    Background,
    Layer(LyricsKind, String),
}

fn collect(nodes: &[Node], role: &Role, base: i64, out: &mut Sink<'_>) {
    for node in nodes {
        match node {
            Node::Text(text) => push(out, role, Piece::plain(collapse(text))),
            Node::Element(element) if element.name == "br" => {
                push(out, role, Piece::plain(" "));
            }
            Node::Element(element) if element.name == "span" => {
                let own;
                let role = match (role, element.attr("role")) {
                    (Role::Layer(..), _) => role,
                    (_, Some("x-bg")) => &Role::Background,
                    (_, Some("x-translation")) => {
                        own = Role::Layer(LyricsKind::Translation, layer_lang(element));
                        &own
                    }
                    (_, Some("x-roman")) => {
                        own = Role::Layer(LyricsKind::Pronunciation, layer_lang(element));
                        &own
                    }
                    _ => role,
                };
                let begin = begin_of(element, base);
                match begin {
                    Some(start) if !matches!(role, Role::Layer(..)) && !holds_timed(element) => {
                        let end = end_of(element, base, begin);
                        let text = collapse(&sung_text(element));
                        push(out, role, Piece::timed(text, start, end));
                    }
                    _ => collect(&element.children, role, begin.unwrap_or(base), out),
                }
            }
            Node::Element(element) => collect(&element.children, role, base, out),
        }
    }
}

fn push(out: &mut Sink<'_>, role: &Role, piece: Piece) {
    let paragraph = &mut *out.paragraph;
    match role {
        Role::Main => paragraph.main.push(piece),
        Role::Background => paragraph.background.push(piece),
        Role::Layer(kind, lang) => {
            if let Some((_, text)) = paragraph.layers.iter_mut().find(|(index, _)| {
                let (known_kind, known_lang) = &out.layers.known[*index];
                known_kind == kind && known_lang == lang
            }) {
                text.push_str(&piece.text);
            } else if !piece.text.trim().is_empty()
                && let Some(index) = out.layers.index(*kind, lang)
            {
                paragraph.layers.push((index, piece.text));
            }
        }
    }
}

/// True when a span below `element` has a begin of its own.
fn holds_timed(element: &Element) -> bool {
    element
        .elements()
        .any(|child| (child.name == "span" && child.attr("begin").is_some()) || holds_timed(child))
}

/// A timed span's sung text: everything below it except layer and
/// background spans.
fn sung_text(element: &Element) -> String {
    let mut out = String::new();
    for node in &element.children {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Element(child) if child.name == "br" => out.push(' '),
            Node::Element(child)
                if child.name == "span"
                    && matches!(
                        child.attr("role"),
                        Some("x-bg" | "x-translation" | "x-roman")
                    ) => {}
            Node::Element(child) => out.push_str(&sung_text(child)),
        }
    }
    out
}

/// An element's `begin`, resolved against `base`.
fn begin_of(element: &Element, base: i64) -> Option<i64> {
    resolve(parse_time(element.attr("begin")?)?, base)
}

/// An element's `end`, else its `begin` plus its `dur`.
fn end_of(element: &Element, base: i64, begin: Option<i64>) -> Option<i64> {
    if let Some(end) = element.attr("end") {
        return resolve(parse_time(end)?, base);
    }
    let length = match parse_time(element.attr("dur")?)? {
        Time::At(ms) | Time::After(ms) => ms,
    };
    resolve(Time::At(begin?.checked_add(length)?), 0)
}

/// A time as written: absolute, or counted from the nearest timed
/// ancestor's begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Time {
    At(i64),
    After(i64),
}

fn resolve(time: Time, base: i64) -> Option<i64> {
    let ms = match time {
        Time::At(ms) => ms,
        Time::After(ms) => base.checked_add(ms)?,
    };
    (0..=MAX_TIME_MS).contains(&ms).then_some(ms)
}

/// One TTML time expression. `None` when it is not one read here.
fn parse_time(raw: &str) -> Option<Time> {
    let raw = raw.trim();
    if raw.contains(':') {
        let parts: Vec<&str> = raw.split(':').collect();
        let (hours, minutes, seconds) = match parts.as_slice() {
            [minutes, seconds] => (0, number(minutes)?, *seconds),
            [hours, minutes, seconds] => (
                number(hours)?,
                number(minutes).filter(|minutes| *minutes < 60)?,
                *seconds,
            ),
            _ => return None,
        };
        let seconds = decimal(seconds, 1_000)?;
        if seconds >= 60_000 {
            return None;
        }
        let ms = hours
            .checked_mul(3_600_000)?
            .checked_add(minutes.checked_mul(60_000)?)?
            .checked_add(seconds)?;
        return Some(Time::At(ms));
    }
    let units: [(&str, i64); 4] = [("ms", 1), ("h", 3_600_000), ("m", 60_000), ("s", 1_000)];
    for (unit, scale) in units {
        if let Some(value) = raw.strip_suffix(unit) {
            return decimal(value, scale).map(Time::After);
        }
    }
    decimal(raw, 1_000).map(Time::At)
}

/// Plain digits, at most [`MAX_TIME_DIGITS`].
fn number(text: &str) -> Option<i64> {
    if text.is_empty()
        || text.len() > MAX_TIME_DIGITS
        || !text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    text.parse().ok()
}

/// `digits[.digits]` times `scale`, the fraction read as digits; fraction
/// digits past what `scale` can hold are dropped.
fn decimal(text: &str, scale: i64) -> Option<i64> {
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (text, ""),
    };
    let mut ms = number(whole)?.checked_mul(scale)?;
    if !fraction.is_empty() {
        number(fraction)?;
        let mut place = scale;
        for digit in fraction.bytes() {
            place /= 10;
            if place == 0 {
                break;
            }
            ms = ms.checked_add(i64::from(digit - b'0') * place)?;
        }
    }
    Some(ms)
}

/// Sort the paragraphs into lines and build the tracks: the main one, then
/// the first [`MAX_LAYERS`] layers that have lines. Refused when the
/// lines' text passes the character cap.
fn assemble(
    timing: Timing,
    lang: Option<String>,
    mut head: Head,
    layers: &Layers,
    paragraphs: Vec<Paragraph>,
) -> Result<Option<Lyrics>, &'static str> {
    let words = timing == Timing::Word;
    let mut keyed: Vec<(i64, Paragraph)> = Vec::with_capacity(paragraphs.len());
    let mut above = i64::MIN;
    for mut paragraph in paragraphs {
        if paragraph.start.is_none() {
            paragraph.start = paragraph
                .main
                .iter()
                .chain(&paragraph.background)
                .find_map(|piece| piece.start_ms);
        }
        if !words {
            for piece in paragraph.main.iter_mut().chain(&mut paragraph.background) {
                piece.start_ms = None;
                piece.end_ms = None;
            }
        }
        let key = paragraph.start.unwrap_or(above);
        above = key;
        keyed.push((key, paragraph));
    }
    keyed.sort_by_key(|(key, _)| *key);
    let paragraphs: Vec<Paragraph> = keyed.into_iter().map(|(_, paragraph)| paragraph).collect();
    let synced = timing != Timing::None && paragraphs.iter().any(|p| p.start.is_some());

    let timed = |pieces: &[Piece]| pieces.iter().any(|piece| piece.start_ms.is_some());
    let declared = |id: &str| head.agents.iter().position(|agent| agent.id == id);
    let main_agent = paragraphs
        .iter()
        .filter(|p| timed(&p.main))
        .find_map(|p| {
            p.agent
                .as_deref()
                .and_then(declared)
                .filter(|index| !head.agents[*index].group)
        })
        .or_else(|| head.agents.iter().position(|agent| !agent.group));
    let mut agents: Vec<Agent> = head
        .agents
        .iter()
        .enumerate()
        .map(|(index, agent)| Agent {
            id: agent.id.clone(),
            role: if Some(index) == main_agent {
                AgentRole::Main
            } else if agent.group {
                AgentRole::Group
            } else {
                AgentRole::Voice
            },
            name: agent.name.clone(),
        })
        .collect();
    let mut main_agent = main_agent;
    let mut background_agent = None;
    if words && paragraphs.iter().any(|p| timed(&p.background)) {
        background_agent = Some(agents.len());
        agents.push(Agent {
            id: free_agent_id(&agents, "bg"),
            role: AgentRole::Bg,
            name: None,
        });
        if main_agent.is_none() {
            main_agent = Some(agents.len());
            agents.push(Agent {
                id: free_agent_id(&agents, "main"),
                role: AgentRole::Main,
                name: None,
            });
        }
    }

    // A head text goes to the first line naming its key; a later line
    // naming the same key gets none, so no text is copied.
    let mut head_texts = std::mem::take(&mut head.texts);
    let mut layer_lines: Vec<Vec<RawLine>> = vec![Vec::new(); layers.known.len()];
    let mut lines = Vec::with_capacity(paragraphs.len());
    for paragraph in paragraphs {
        let mut inline = paragraph.layers;
        for (index, layer) in layer_lines.iter_mut().enumerate() {
            let from_head = paragraph.key.as_ref().and_then(|key| {
                head_texts
                    .get_mut(index)
                    .and_then(|texts| texts.remove(key))
            });
            let from_inline = || {
                inline
                    .iter_mut()
                    .find(|(known, _)| *known == index)
                    .map(|(_, text)| std::mem::take(text))
            };
            if let Some(text) = from_head
                .or_else(from_inline)
                .filter(|text| !text.trim().is_empty())
            {
                layer.push(RawLine::single(
                    paragraph.start,
                    paragraph.end,
                    vec![Piece::plain(text)],
                ));
            }
        }
        let agent = paragraph.agent.as_deref().and_then(declared).or(main_agent);
        let mut parts = vec![RawPart {
            agent,
            pieces: paragraph.main,
        }];
        if !paragraph.background.is_empty() {
            parts.push(RawPart {
                agent: background_agent,
                pieces: paragraph.background,
            });
        }
        lines.push(RawLine {
            start_ms: paragraph.start,
            end_ms: paragraph.end,
            parts,
        });
    }

    let over = |_| "lines past the character cap";
    let mut budget = Budget::new();
    let main = build_track(
        RawTrack {
            kind: LyricsKind::Main,
            lang,
            synced,
            agents,
            lines,
        },
        &mut budget,
    )
    .map_err(over)?;
    let mut built = Vec::new();
    for ((kind, lang), lines) in layers.known.iter().zip(layer_lines) {
        if lines.is_empty() || built.len() >= MAX_LAYERS {
            continue;
        }
        let track = RawTrack {
            kind: *kind,
            lang: Some(lang.clone()),
            synced,
            agents: Vec::new(),
            lines,
        };
        built.push(build_track(track, &mut budget).map_err(over)?);
    }
    Ok(Lyrics::with_text(main, built))
}
