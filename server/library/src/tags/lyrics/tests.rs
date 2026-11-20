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
