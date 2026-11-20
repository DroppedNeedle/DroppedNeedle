use super::*;

/// Each main line's text and start.
fn lines(lyrics: &Lyrics) -> Vec<(&str, Option<i64>)> {
    lyrics
        .main
        .lines()
        .iter()
        .map(|line| (line.text(), line.start_ms()))
        .collect()
}

/// LRC stamps parse to milliseconds, plain text drops empty lines,
/// the byte cap holds, and a `.lrc` reads as Jellyfin reads it: a line
/// stamped twice is sung twice, in time order, and a stamped blank line
/// stays, so the line before it stops showing through the gap. Lines
/// without word stamps carry no cues and no agents.
#[test]
fn lyrics_parse_and_cap() {
    let synced = parse_lyrics_text(
        "[ar:Someone]\n[01:02.34] hello\n[00:00.5] x\n[99:99.99] y\nno\tstamp\n[0:5.00]z",
    )
    .unwrap()
    .unwrap();
    assert!(synced.main.synced());
    assert_eq!(
        lines(&synced),
        [
            ("x", Some(500)),
            ("no stamp", None),
            ("z", Some(5_000)),
            ("hello", Some(62_340)),
        ]
    );
    assert!(synced.layers.is_empty());
    assert!(synced.main.agents().is_empty());
    assert!(
        synced
            .main
            .lines()
            .iter()
            .all(|line| line.cue_lines().is_empty() && line.end_ms().is_none())
    );

    let plain = parse_lyrics_text("One\n\n  Two  ").unwrap().unwrap();
    assert!(!plain.main.synced());
    assert_eq!(lines(&plain), [("One", None), ("Two", None)]);
    assert_eq!(plain.main.kind(), LyricsKind::Main);
    assert_eq!(plain.main.lang(), None);
    assert_eq!(parse_lyrics_text(" \n[00:01.00]\n").unwrap(), None);
    assert_eq!(
        parse_lyrics(Some(&"x".repeat(MAX_LYRICS_BYTES + 1)), None),
        Err(LyricsRefused::TooManyBytes)
    );
    assert_eq!(
        parse_lyrics(Some(&"é".repeat(MAX_LYRICS_CHARACTERS + 1)), None),
        Err(LyricsRefused::TooManyCharacters)
    );

    let dir = droppedneedle_kernel::test_support::ScratchDir::new("lyrics-lrc").expect("scratch");
    std::fs::write(
        dir.path().join("song.lrc"),
        "[ar:Someone]\n[00:12.00][00:45.00]Chorus\n[00:20.00]\n",
    )
    .expect("lrc writes");
    let song = dir.path().join("song.flac");
    let (source, lyrics) =
        read_file_lyrics(TrackFile::new(dir.path(), &song)).expect("the sidecar reads");
    assert_eq!(source, LyricsSource::Sidecar);
    assert!(lyrics.main.synced());
    assert_eq!(
        lines(&lyrics),
        [
            ("Chorus", Some(12_000)),
            ("", Some(20_000)),
            ("Chorus", Some(45_000)),
        ]
    );
}

/// Lines given to the builder directly are sorted by start, stably, a
/// line with no start staying after the line above it.
#[test]
fn given_lines_sort_by_start() {
    let line = |start_ms: Option<i64>, text: &str| LineInput {
        start_ms,
        end_ms: None,
        pieces: vec![Piece::plain(text)],
    };
    let sorted = Lyrics::from_lines(vec![
        line(Some(2_000), "two"),
        line(None, "after two"),
        line(Some(1_000), "one"),
        line(Some(2_000), "two again"),
    ])
    .unwrap();
    assert_eq!(
        lines(&sorted),
        [
            ("one", Some(1_000)),
            ("two", Some(2_000)),
            ("after two", None),
            ("two again", Some(2_000)),
        ]
    );
}

/// A word with no end before many lines that all start earlier is
/// settled by a search, not a walk over every later line, so the build
/// stays quick.
#[test]
fn unended_words_before_many_lines_stay_quick() {
    let lines: Vec<LineInput> = (0..50_000)
        .map(|_| LineInput {
            start_ms: Some(0),
            end_ms: None,
            pieces: vec![Piece::timed("a", 5_940_000, None)],
        })
        .collect();
    let started = std::time::Instant::now();
    let lyrics = Lyrics::from_lines(lines).unwrap();
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(10), "took {took:?}");
    assert!(
        lyrics
            .main
            .lines()
            .iter()
            .all(|line| line.cue_lines().is_empty())
    );
}

/// One line: its text, start, end and each cue as start, end and the
/// first and last byte it covers.
type Shown<'a> = (
    &'a str,
    Option<i64>,
    Option<i64>,
    Vec<(i64, i64, usize, usize)>,
);

/// Each main line as [`Shown`], the main cue line's cues only.
fn shown(lyrics: &Lyrics) -> Vec<Shown<'_>> {
    lyrics
        .main
        .lines()
        .iter()
        .map(|line| {
            let cues = line.cue_lines().first().map_or_else(Vec::new, |cue_line| {
                assert_eq!(cue_line.text(), line.text(), "one part: the line's text");
                cue_line
                    .cues()
                    .iter()
                    .map(|cue| {
                        (
                            cue.start_ms(),
                            cue.end_ms(),
                            cue.byte_start(),
                            cue.byte_end(),
                        )
                    })
                    .collect()
            });
            (line.text(), line.start_ms(), line.end_ms(), cues)
        })
        .collect()
}

/// Enhanced LRC: `<..>` word stamps become cues over exactly their word
/// (inclusive UTF-8 byte offsets), never stay in the text, and a `<..>`
/// that is not a stamp is text. A trailing stamp ends the line; a missing
/// end comes from the next stamp, else the first later line starting at or
/// after the word (an overlapping duet line is skipped), else the cues go.
#[test]
fn enhanced_lrc_reads_word_timings() {
    let text = "[lang:en-GB]\n\
        [00:01.00]<00:01.00>Héllo <00:01.50>wörld <00:02.00>\n\
        [00:03.00]Alpha <00:03.40>beta\n\
        [00:03.20]<00:03.20>duet\n\
        [00:05.00]plain line\n\
        [00:06.00][00:06.50]multi <00:06.10>stamp\n\
        [00:07.00]I <3 you <00:07.50>later\n\
        [00:08.00]<00:08.40>back<00:08.20>ward\n\
        [00:10.00]late <00:09.50>\n\
        <00:11.00>lead <00:11.50>only\n\
        pre <00:12.00>mid\n\
        [00:13.00]<00:13.00>tab\t<00:13.20>c\u{1}trl\n\
        [0:14.5]<0:14.5>x <00:14.80>\n\
        [00:15.00]<00:15.00>final\n";
    let lyrics = parse_lyrics_text(text).unwrap().unwrap();
    assert!(lyrics.main.synced());
    assert_eq!(lyrics.main.lang(), Some("en-GB"));
    assert!(lyrics.main.agents().is_empty());
    assert_eq!(
        shown(&lyrics),
        [
            (
                "Héllo wörld",
                Some(1_000),
                Some(2_000),
                vec![(1_000, 1_500, 0, 5), (1_500, 2_000, 7, 12)]
            ),
            (
                "Alpha beta",
                Some(3_000),
                None,
                vec![(3_000, 3_400, 0, 4), (3_400, 5_000, 6, 9)]
            ),
            ("duet", Some(3_200), None, vec![(3_200, 5_000, 0, 3)]),
            ("plain line", Some(5_000), None, vec![]),
            ("multi stamp", Some(6_000), None, vec![]),
            ("multi stamp", Some(6_500), None, vec![]),
            (
                "I <3 you later",
                Some(7_000),
                None,
                vec![(7_000, 7_500, 0, 7), (7_500, 8_000, 9, 13)]
            ),
            ("backward", Some(8_000), None, vec![]),
            ("late", Some(10_000), None, vec![]),
            (
                "lead only",
                Some(11_000),
                None,
                vec![(11_000, 11_500, 0, 3), (11_500, 12_000, 5, 8)]
            ),
            ("pre mid", Some(12_000), None, vec![(12_000, 13_000, 4, 6)]),
            (
                "tab ctrl",
                Some(13_000),
                None,
                vec![(13_000, 13_200, 0, 2), (13_200, 14_500, 4, 7)]
            ),
            (
                "x",
                Some(14_500),
                Some(14_800),
                vec![(14_500, 14_800, 0, 0)]
            ),
            ("final", Some(15_000), None, vec![]),
        ]
    );
    let (start, end) = lyrics.main.lines()[0].cue_lines()[0].span(Some(2_000));
    assert_eq!((start, end), (1_000, 2_000));

    // A file of word-led lines alone is synced; plain text loses its stamps.
    let led = parse_lyrics_text("plain words\n<00:01.00>only <00:02.00>words")
        .unwrap()
        .unwrap();
    assert!(led.main.synced());
    assert_eq!(
        lines(&led),
        [("plain words", None), ("only words", Some(1_000))]
    );
    let plain = parse_lyrics(Some("one <00:01.00>two\n<00:02.00>"), None)
        .unwrap()
        .unwrap();
    assert!(!plain.main.synced());
    assert_eq!(shown(&plain), [("one two", None, None, vec![])]);

    // `[offset:]` counts wherever it sits; a positive one shows lines
    // sooner, never before 0.
    let shifted = parse_lyrics_text(
        "[00:02.00]<00:02.00>a <00:02.50>b <00:03.00>\n[00:00.20]early\n[offset:+500]",
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        shown(&shifted),
        [
            ("early", Some(0), None, vec![]),
            (
                "a b",
                Some(1_500),
                Some(2_500),
                vec![(1_500, 2_000, 0, 0), (2_000, 2_500, 2, 2)]
            ),
        ]
    );

    // Given ends past the next cue's start are cut back to it; a cue
    // line ends at the later of the line's end and its last cue's.
    let given = Lyrics::from_lines(vec![LineInput {
        start_ms: Some(0),
        end_ms: Some(900),
        pieces: vec![
            Piece::timed("one ", 0, Some(600)),
            Piece::timed("two", 500, Some(1_000)),
        ],
    }])
    .unwrap();
    assert_eq!(
        shown(&given),
        [(
            "one two",
            Some(0),
            Some(900),
            vec![(0, 500, 0, 2), (500, 1_000, 4, 6)]
        )]
    );
    assert_eq!(
        given.main.lines()[0].cue_lines()[0].span(Some(900)),
        (0, 1_000)
    );
}

/// A line stamped many times is copied once per stamp; copies that would
/// pass the character cap are refused before they are made.
#[test]
fn stamp_copies_past_the_cap_are_refused() {
    let text = format!("{}{}", "[00:01.00]".repeat(2_000), "x".repeat(1_000));
    assert_eq!(
        parse_lyrics_text(&text),
        Err(LyricsRefused::TooManyCharacters)
    );
}
