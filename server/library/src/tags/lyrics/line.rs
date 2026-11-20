//! The one line builder every lyrics source goes through.
//!
//! A source hands over lines of pieces: runs of text, each with a start
//! and end when the source times it. The builder joins them into the
//! line's text and turns timed pieces into word cues, holding the same
//! rules for every source:
//!
//! - Text: C0 controls are dropped and tab, CR and LF become spaces before
//!   any offset is taken. A piece's leading whitespace is dropped when the
//!   text so far ends in whitespace, and the line is trimmed.
//! - Times: a negative time, or one past 100 hours, is untimed. A line's
//!   end is kept only with a start and when not before it. A line with no
//!   start of its own takes its first timed piece's.
//! - Cues: each timed piece with text is a cue covering exactly its
//!   trimmed text; a timed piece of only whitespace gives no cue, but its
//!   start ends the cue before. A missing end is the next timed piece's
//!   start; the last cue's is the line's end, else the start of the first
//!   later line that starts at or after it. A cue line whose ends cannot
//!   all be filled, whose starts go backwards or with an end before its
//!   start loses its cues (the line keeps its text and start). An end past
//!   the next cue's start is cut back to it.
//! - Agents: kept only when a line has more than one cue line or cue lines
//!   name two or more agents, and then exactly one is `Main` and every cue
//!   line names one. Otherwise they are cleared with every reference.
//! - Size: the text of the lines a source builds, main and layers
//!   together, is held to [`MAX_LYRICS_CHARACTERS`] through one [`Budget`];
//!   past it the source is refused.

use std::borrow::Cow;

use super::{
    Agent, AgentRole, Cue, CueLine, LyricLine, Lyrics, LyricsKind, LyricsTrack,
    MAX_LYRICS_CHARACTERS,
};

/// Latest time read, in milliseconds: 100 hours. A time past it is untimed.
pub const MAX_TIME_MS: i64 = 100 * 3_600 * 1_000;

/// A run of a line's text, timed or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    /// The text as the source gives it; the builder cleans it.
    pub text: String,
    /// When the piece starts, in milliseconds. `None` for plain text.
    pub start_ms: Option<i64>,
    /// When it ends, in milliseconds, when the source says.
    pub end_ms: Option<i64>,
}

impl Piece {
    /// Text with no timing.
    #[must_use]
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            start_ms: None,
            end_ms: None,
        }
    }

    /// Text sung from `start_ms`, to `end_ms` when known.
    #[must_use]
    pub fn timed(text: impl Into<String>, start_ms: i64, end_ms: Option<i64>) -> Self {
        Self {
            text: text.into(),
            start_ms: Some(start_ms),
            end_ms,
        }
    }
}

/// One line as a source reads it: its pieces in order and its own times.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    /// The line's start, in milliseconds. `None` takes the first timed
    /// piece's.
    pub start_ms: Option<i64>,
    /// The line's end, in milliseconds.
    pub end_ms: Option<i64>,
    /// The line's text, in order.
    pub pieces: Vec<Piece>,
}

impl Lyrics {
    /// Main lyrics from lines of pieces, through the line builder. Lines
    /// are sorted by start, stably; a line with no start stays after the
    /// timed line above it. Synced when any line or piece has a start.
    /// `None` when no line has text, or when the lines' text passes
    /// [`MAX_LYRICS_CHARACTERS`].
    #[must_use]
    pub fn from_lines(lines: Vec<LineInput>) -> Option<Self> {
        let synced = lines.iter().any(|line| {
            line.start_ms.is_some() || line.pieces.iter().any(|piece| piece.start_ms.is_some())
        });
        let lines = sort_by_start(
            lines
                .into_iter()
                .map(|line| RawLine::single(line.start_ms, line.end_ms, line.pieces))
                .collect(),
        );
        let main = build_track(
            RawTrack {
                kind: LyricsKind::Main,
                lang: None,
                synced,
                agents: Vec::new(),
                lines,
            },
            &mut Budget::new(),
        )
        .ok()?;
        Self::with_text(main, Vec::new())
    }

    /// The lyrics, or `None` when no main line has text.
    pub(super) fn with_text(main: LyricsTrack, layers: Vec<LyricsTrack>) -> Option<Self> {
        main.lines
            .iter()
            .any(|line| !line.text.is_empty())
            .then_some(Self { main, layers })
    }
}

/// A line before building: its times and its parts. The first part is
/// the main one; later parts (background vocals) share the line's index.
#[derive(Debug, Clone, Default)]
pub(super) struct RawLine {
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub parts: Vec<RawPart>,
}

impl RawLine {
    /// A line of one part with no agent.
    pub fn single(start_ms: Option<i64>, end_ms: Option<i64>, pieces: Vec<Piece>) -> Self {
        Self {
            start_ms,
            end_ms,
            parts: vec![RawPart {
                agent: None,
                pieces,
            }],
        }
    }

    /// The line's start: its own, else its first timed piece's.
    pub fn effective_start(&self) -> Option<i64> {
        time(self.start_ms).or_else(|| {
            self.parts
                .iter()
                .flat_map(|part| &part.pieces)
                .find_map(|piece| time(piece.start_ms))
        })
    }
}

/// One part of a line, sung by one agent.
#[derive(Debug, Clone, Default)]
pub(super) struct RawPart {
    /// Index into the track's agents.
    pub agent: Option<usize>,
    pub pieces: Vec<Piece>,
}

/// A track before building.
#[derive(Debug, Clone)]
pub(super) struct RawTrack {
    pub kind: LyricsKind,
    pub lang: Option<String>,
    pub synced: bool,
    pub agents: Vec<Agent>,
    pub lines: Vec<RawLine>,
}

/// Sort lines by start, stably: a line with no start sorts with the line
/// above it.
pub(super) fn sort_by_start(lines: Vec<RawLine>) -> Vec<RawLine> {
    let mut above = i64::MIN;
    let mut keyed: Vec<(i64, RawLine)> = lines
        .into_iter()
        .map(|line| {
            above = line.effective_start().unwrap_or(above);
            (above, line)
        })
        .collect();
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, line)| line).collect()
}

/// What is left of the text a source may produce, in characters.
#[derive(Debug)]
pub(super) struct Budget {
    left: usize,
}

/// A source's text went past [`MAX_LYRICS_CHARACTERS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OverBudget;

impl Budget {
    /// [`MAX_LYRICS_CHARACTERS`] to spend.
    pub fn new() -> Self {
        Self {
            left: MAX_LYRICS_CHARACTERS,
        }
    }

    /// Spend `chars`, or fail once the budget would run out.
    pub fn spend(&mut self, chars: usize) -> Result<(), OverBudget> {
        self.left = self.left.checked_sub(chars).ok_or(OverBudget)?;
        Ok(())
    }
}

/// The characters of `pieces`' text.
pub(super) fn chars_of(pieces: &[Piece]) -> usize {
    pieces
        .iter()
        .map(|piece| piece.text.chars().count())
        .fold(0, usize::saturating_add)
}

/// A time in range, else untimed.
fn time(ms: Option<i64>) -> Option<i64> {
    ms.filter(|ms| (0..=MAX_TIME_MS).contains(ms))
}

/// An id not yet taken by `agents`: `base`, else `base` with the first
/// free number after it.
pub(super) fn free_agent_id(agents: &[Agent], base: &str) -> String {
    let taken = |id: &str| agents.iter().any(|agent| agent.id == id);
    if !taken(base) {
        return base.to_owned();
    }
    (2_usize..)
        .map(|n| format!("{base}{n}"))
        .find(|id| !taken(id))
        .unwrap_or_else(|| base.to_owned())
}

/// Build one track, its lines' text spent from `budget`. Lines keep their
/// order; a source sorts them by start first.
pub(super) fn build_track(raw: RawTrack, budget: &mut Budget) -> Result<LyricsTrack, OverBudget> {
    let synced = raw.synced;
    let mut lines = raw.lines;
    if !synced {
        for line in &mut lines {
            line.start_ms = None;
            line.end_ms = None;
            for piece in line.parts.iter_mut().flat_map(|part| &mut part.pieces) {
                piece.start_ms = None;
                piece.end_ms = None;
            }
        }
    }
    let starts: Vec<Option<i64>> = lines.iter().map(RawLine::effective_start).collect();
    // The timed lines, in order. Sources sort lines by start, so their
    // starts never go down and a binary search finds the first later line
    // starting at or after a time.
    let timed_starts: Vec<(usize, i64)> = starts
        .iter()
        .enumerate()
        .filter_map(|(index, start)| Some((index, (*start)?)))
        .collect();
    let mut built: Vec<LyricLine> = Vec::with_capacity(lines.len());
    for (index, line) in lines.into_iter().enumerate() {
        let start_ms = starts[index];
        let end_ms = time(line.end_ms).filter(|end| start_ms.is_some_and(|start| *end >= start));
        let after = &timed_starts[timed_starts.partition_point(|(at, _)| *at <= index)..];
        let later = |from: i64| {
            let at = after.partition_point(|(_, start)| *start < from);
            after
                .get(at)
                .map(|(_, start)| *start)
                .filter(|start| *start >= from)
        };
        let mut texts: Vec<String> = Vec::new();
        let mut cue_lines = Vec::new();
        for part in line.parts {
            let (text, timed) = join(&part.pieces);
            if text.is_empty() {
                continue;
            }
            if let Some(cues) = cues(&timed, end_ms, &later) {
                cue_lines.push(CueLine {
                    agent: part.agent,
                    text: text.clone(),
                    cues,
                });
            }
            texts.push(text);
        }
        let text = texts.join(" ");
        budget.spend(text.chars().count())?;
        built.push(LyricLine {
            text,
            start_ms,
            end_ms,
            cue_lines,
        });
    }
    let agents = settle_agents(raw.agents, &mut built);
    Ok(LyricsTrack {
        kind: raw.kind,
        lang: raw.lang,
        synced,
        agents,
        lines: built,
    })
}

/// A timed piece after joining: its start and end, and the bytes of the
/// line's text it covers (`None` for whitespace only).
struct Timed {
    start: i64,
    end: Option<i64>,
    bytes: Option<(usize, usize)>,
}

/// Join a part's pieces into its trimmed text and its timed pieces.
fn join(pieces: &[Piece]) -> (String, Vec<Timed>) {
    let mut text = String::new();
    let mut timed = Vec::new();
    for piece in pieces {
        let cleaned = clean(&piece.text);
        let mut added: &str = &cleaned;
        if text.ends_with(char::is_whitespace) {
            added = added.trim_start();
        }
        let at = text.len();
        text.push_str(added);
        let Some(start) = time(piece.start_ms) else {
            continue;
        };
        let inner = added.trim_start();
        let lead = added.len() - inner.len();
        let inner = inner.trim_end();
        timed.push(Timed {
            start,
            end: time(piece.end_ms),
            bytes: (!inner.is_empty()).then(|| (at + lead, at + lead + inner.len() - 1)),
        });
    }
    let kept = text.trim_start();
    let shift = text.len() - kept.len();
    let kept = kept.trim_end().to_owned();
    for piece in &mut timed {
        if let Some((first, last)) = piece.bytes {
            piece.bytes = Some((first - shift, last - shift));
        }
    }
    (kept, timed)
}

/// Drop C0 controls; tab, CR and LF become spaces.
fn clean(text: &str) -> Cow<'_, str> {
    if !text.contains(|c: char| c.is_ascii_control() && c != '\u{7f}') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .filter_map(|c| match c {
                '\t' | '\r' | '\n' => Some(' '),
                '\u{0}'..='\u{1f}' => None,
                other => Some(other),
            })
            .collect(),
    )
}

/// The cues of one part, or `None` when it has none or they break a rule.
/// `later` gives the start of the first later line starting at or after a
/// time.
fn cues(
    timed: &[Timed],
    line_end: Option<i64>,
    later: &dyn Fn(i64) -> Option<i64>,
) -> Option<Vec<Cue>> {
    let mut cues: Vec<Cue> = Vec::new();
    for (index, piece) in timed.iter().enumerate() {
        let Some((byte_start, byte_end)) = piece.bytes else {
            continue;
        };
        let end = match (piece.end, timed.get(index + 1)) {
            (Some(end), _) => end,
            (None, Some(next)) => next.start,
            (None, None) => line_end
                .filter(|end| *end >= piece.start)
                .or_else(|| later(piece.start))?,
        };
        cues.push(Cue {
            start_ms: piece.start,
            end_ms: end,
            byte_start,
            byte_end,
        });
    }
    if cues.is_empty() {
        return None;
    }
    for index in 0..cues.len() {
        if let Some(next) = cues.get(index + 1).map(|next| next.start_ms) {
            if next < cues[index].start_ms {
                return None;
            }
            if next < cues[index].end_ms {
                cues[index].end_ms = next;
            }
        }
        if cues[index].end_ms < cues[index].start_ms {
            return None;
        }
    }
    Some(cues)
}

/// Keep the agents only where they say something, and hold their rules.
fn settle_agents(mut agents: Vec<Agent>, lines: &mut [LyricLine]) -> Vec<Agent> {
    let count = agents.len();
    for cue_line in lines.iter_mut().flat_map(|line| &mut line.cue_lines) {
        cue_line.agent = cue_line.agent.filter(|agent| *agent < count);
    }
    let mut named: Vec<usize> = lines
        .iter()
        .flat_map(|line| &line.cue_lines)
        .filter_map(|cue_line| cue_line.agent)
        .collect();
    named.sort_unstable();
    named.dedup();
    let keep = named.len() >= 2 || lines.iter().any(|line| line.cue_lines.len() > 1);
    if !keep {
        for cue_line in lines.iter_mut().flat_map(|line| &mut line.cue_lines) {
            cue_line.agent = None;
        }
        return Vec::new();
    }
    let mut main = None;
    for (index, agent) in agents.iter_mut().enumerate() {
        if agent.role == AgentRole::Main {
            if main.is_some() {
                agent.role = AgentRole::Voice;
            } else {
                main = Some(index);
            }
        }
    }
    let main = match main {
        Some(main) => main,
        None => match agents
            .iter()
            .position(|agent| agent.role == AgentRole::Voice)
        {
            Some(voice) => {
                agents[voice].role = AgentRole::Main;
                voice
            }
            None => {
                agents.push(Agent {
                    id: free_agent_id(&agents, "main"),
                    role: AgentRole::Main,
                    name: None,
                });
                agents.len() - 1
            }
        },
    };
    let count = agents.len();
    let mut used = vec![false; count];
    used[main] = true;
    for cue_line in lines.iter_mut().flat_map(|line| &mut line.cue_lines) {
        let agent = cue_line
            .agent
            .filter(|agent| *agent < count)
            .unwrap_or(main);
        cue_line.agent = Some(agent);
        used[agent] = true;
    }
    let mut renumber = vec![0; count];
    let mut kept = Vec::new();
    for (index, agent) in agents.into_iter().enumerate() {
        if used[index] {
            renumber[index] = kept.len();
            kept.push(agent);
        }
    }
    for cue_line in lines.iter_mut().flat_map(|line| &mut line.cue_lines) {
        cue_line.agent = cue_line.agent.map(|agent| renumber[agent]);
    }
    kept
}
