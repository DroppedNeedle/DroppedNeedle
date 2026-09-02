"""Different-album discriminator shared by the Usenet + Soulseek scorers.

``names_different_album`` keeps a request for one album from matching every OTHER album by
the same artist (the Led Zeppelin debut vs II/III/IV, Houses of the Holy, In Through the
Out Door...) - the failure that made the picker burn a download per wrong album and exhaust
before reaching the one requested. Cases below are drawn from real Newznab/Soulseek titles.
"""

import pytest

from services.native.title_match import (
    _tokens,
    artist_evidence,
    fold,
    names_different_album,
    strip_featuring,
    title_containment_score,
)


def test_fold_accents_and_case_but_keeps_cjk():
    assert fold("Mötley Crüe") == "motley crue"
    assert fold("Sigur Rós") == "sigur ros"
    assert fold("Beyoncé") == "beyonce"
    assert fold("林宥嘉 神秘嘉宾") == "林宥嘉 神秘嘉宾"  # CJK left intact, not romanised


def test_strip_featuring():
    assert strip_featuring("Crazy in Love (feat. Jay-Z)") == "Crazy in Love"
    assert strip_featuring("Song ft. Someone") == "Song"
    assert strip_featuring("Soft Machine") == "Soft Machine"  # 'ft' inside a word is untouched


def test_accented_artist_is_recognised_so_the_guard_still_discriminates():
    # Before accent-folding the guard failed OPEN on accented artists (artist "not present"),
    # so it stopped rejecting wrong albums for them. Now it discriminates correctly.
    assert names_different_album("Dr. Feelgood", "Mötley Crüe", "Motley Crue - Theatre of Pain (1985) [FLAC]")
    assert not names_different_album("Dr. Feelgood", "Mötley Crüe", "Motley_Crue-Dr_Feelgood-LP-FLAC-1989-GRP")


def test_featured_artist_is_not_read_as_a_foreign_album_word():
    # The featured artist must not count as an extra album word that wrongly rejects the match.
    assert not names_different_album(
        "Crazy in Love", "Beyoncé", "Beyonce - Crazy in Love (feat. Jay-Z) [FLAC]"
    )

_LZ = "Led Zeppelin"  # self-titled: album == artist, the hard case (album adds no discriminator)


def test_self_titled_debut_rejects_other_studio_albums():
    # The exact failing titles: every one is a DIFFERENT album, not the requested debut.
    for cand in (
        "led_zeppelin-led_zeppelin_ii-lp-32bit-wavpack-1969-reetkever",
        "Led_Zeppelin-In_Through_The_Out_Door-LP-24BIT-FLAC-1979-REETKEVER",
        "Led_Zeppelin-Houses_Of_The_Holy-LP-32BIT-WAVPACK-1973-REETKEVER",
        "Led_Zeppelin-Physical_Graffiti-2LP-24BIT-FLAC-1975-REETKEVER",
        "Led_Zeppelin-Presence-LP-US-Edition-24BIT-FLAC-1976-BITOCUL",
        "Led_Zeppelin-Coda-LP-32BIT-WAVPACK-1982-REETKEVER",
    ):
        assert names_different_album(_LZ, _LZ, cand), cand


def test_self_titled_debut_accepts_the_debut_including_editions():
    # Clean release, a year-suffixed name, and a deluxe/remaster are all the requested album.
    for cand in (
        "Led_Zeppelin-Led_Zeppelin-LP-24BIT-FLAC-1968-REETKEVER",
        "Led_Zeppelin-Led_Zeppelin_1969-CD-FLAC-1994-GP-FLAC",
        "Led_Zeppelin-Led_Zeppelin-24-96-WEB-FLAC-REMASTERED_DELUXE_EDITION-2014-GP-FLAC",
        "Led_Zeppelin-Led_Zeppelin-Remastered_Deluxe_Edition-2CD-FLAC-2014-GP-FLAC",
    ):
        assert not names_different_album(_LZ, _LZ, cand), cand


def test_usenet_part_counter_prefix_is_stripped():
    # The ``[002/113] "..."`` part counter's digits must not trip the format boundary at
    # position 0 (which would blank the album and wrongly accept every release).
    assert names_different_album(
        _LZ, _LZ, '[002/113] "Led_Zeppelin-Led_Zeppelin_II-LP-32BIT-WAVPACK-1969-REETKEVER.part001.rar"'
    )
    assert not names_different_album(
        _LZ, _LZ, '[002/112] "Led_Zeppelin-Led_Zeppelin-LP-24BIT-FLAC-1968-REETKEVER.part001.rar"'
    )


def test_requested_numbered_album_keeps_its_own_and_rejects_another():
    assert not names_different_album("Led Zeppelin IV", _LZ, "Led_Zeppelin-Led_Zeppelin_IV-LP-FLAC-1971-REETKEVER")
    assert names_different_album("Led Zeppelin IV", _LZ, "Led_Zeppelin-Led_Zeppelin_II-LP-FLAC-1969-REETKEVER")


def test_obfuscated_release_passes_artist_absent():
    # Q4: a fully obfuscated title (no readable artist) is left alone - the indexer-match base
    # score + import tag-match settle it, so it must NOT be rejected here.
    assert not names_different_album("Led Zeppelin IV", _LZ, "aHR0cHM6 scrambled xQ.part01.rar")


def test_missing_marker_is_not_rejected_so_obfuscated_numbered_album_passes():
    # Requesting a numbered album must not reject an artist-named release that omits the numeral.
    assert not names_different_album("Led Zeppelin IV", _LZ, "Led Zeppelin (1971) [FLAC]")


def test_artist_numbered_in_its_name_does_not_reject_its_own_album():
    # The roman in the ARTIST name is part of the artist, so it can't read as a foreign word.
    assert not names_different_album("Some Album", "Apollo IV", "Apollo IV - Some Album [FLAC]")


def test_different_named_album_rejected_for_named_request():
    # A normal (non-self-titled) request still rejects a different album by the same artist.
    assert names_different_album("Houses of the Holy", _LZ, "Led_Zeppelin-Physical_Graffiti-2LP-FLAC-1975")
    assert not names_different_album(
        "Houses of the Holy", _LZ, "Led_Zeppelin-Houses_Of_The_Holy-Remastered_Deluxe_Edition-2CD-FLAC-2014"
    )


def test_numeric_album_title_matches_only_its_own():
    # A digit-only title ("1989") has no album WORDS; only a same-named release (no foreign
    # words) matches, a differently-named one is rejected.
    assert not names_different_album("1989", "Taylor Swift", "Taylor_Swift-1989-CD-FLAC-2014-GROUP")
    assert names_different_album("1989", "Taylor Swift", "Taylor_Swift-Red-CD-FLAC-2012-GROUP")


def test_soulseek_folder_style_directory():
    # Soulseek parents are "Artist Album Year" / "Artist - Album" folders, not scene names.
    assert not names_different_album("OK Computer", "Radiohead", "Radiohead OK Computer 1997")
    assert names_different_album("OK Computer", "Radiohead", "Radiohead - In Rainbows")


def test_edition_version_descriptors_are_not_a_different_album():
    # Regression (step 3 verify): rip album tags routinely carry these; they're the SAME album,
    # not a different one. None may read as a foreign album word.
    for suffix in [
        "Deluxe Version", "Remastered Version", "Explicit", "Clean", "Extended Edition",
        "Standard Edition", "Promo", "Disc 2", "Deluxe", "Special Edition",
    ]:
        assert not names_different_album(
            "Born This Way", "Lady Gaga", f"Lady Gaga Born This Way ({suffix})"
        ), suffix


def test_numbered_sequel_still_rejected_after_edition_additions():
    # The discrimination that matters must still hold.
    assert names_different_album("Led Zeppelin", "Led Zeppelin", "Led Zeppelin Led Zeppelin II")
    assert names_different_album("OK Computer", "Radiohead", "Radiohead Kid A")


# title_containment_score: names the expected title AND NOTHING ELSE (P2/P3.4)


def test_containment_exact_and_numbered():
    assert title_containment_score("the arrival", "the arrival") == 1.0
    assert title_containment_score("the arrival", "01 - the arrival") == 1.0  # digits never extra


def test_containment_edition_tokens_are_not_extra():
    assert title_containment_score("the arrival", "the arrival (Deluxe)") == 1.0
    assert title_containment_score("the arrival", "the arrival [Remastered Version]") == 1.0


def test_containment_penalises_the_incident_pair():
    # token_set_ratio scores this 0.78 (it ignores "in ashford") - the exact hole
    # the wrong single slipped through. Containment reads it as a different work.
    score = title_containment_score("the arrival", "02. Arrival in Ashford")
    assert score == pytest.approx(0.5)


def test_containment_penalises_the_other_live_false_matches():
    # both were real tier=auto candidates in the incident's search job
    assert title_containment_score(
        "the arrival", "07. Arrival - The Waking Hour"
    ) == pytest.approx(1 / 3)
    assert title_containment_score(
        "the arrival", "05 Throbbing Gristle - Dead on Arrival"
    ) < 0.5  # gristle/throbbing/dead all foreign ("the"/"on" are stopwords)


def test_containment_classical_long_titles_stay_strong():
    score = title_containment_score(
        "Symphony No. 9 in D minor, Op. 125",
        "Symphony No. 9 in D Minor, Op. 125 'Choral'",
    )
    assert score >= 0.8  # one extra word on a long title is not a different work


def test_containment_ignore_set_excludes_artist_words_in_filenames():
    with_artist = "01 - Yan Qing - the arrival"
    assert title_containment_score("the arrival", with_artist) < 1.0
    assert title_containment_score(
        "the arrival", with_artist, ignore=frozenset({"yan", "qing"})
    ) == 1.0


def test_containment_degenerate_title_falls_back_to_fuzzy():
    # single-character title: no distinctive words to contain
    assert title_containment_score("X", "X") == 1.0


def test_containment_missing_expected_words_lower_coverage():
    assert title_containment_score("Houses of the Holy", "Houses") == pytest.approx(0.5)


# artist_evidence: the tier='auto' identity gate (D2, 2026-07-05 incident)


def test_artist_evidence_incident_wrong_artist_path_is_not_evidence():
    # The exact path that auto-accepted the wrong single: no trace of "Yan Qing".
    path = (
        "@@yuqfj\\Fab \\Dan Romer\\Dan Romer - A Knight of the Seven Kingdoms "
        "(Season 1)_2026_FLAC 24bit-48kHz\\02. Arrival in Ashford.flac"
    )
    assert not artist_evidence("Yan Qing", path)


def test_artist_evidence_bare_obfuscated_folder_is_unknown_not_evidence():
    # R4 pin: _artist_from_path fabricates the target artist for folders without
    # "Artist - Album"; evidence must treat an artist-less path as UNKNOWN (False),
    # never as a self-match.
    assert not artist_evidence("Yan Qing", "@@abc\\the arrival\\the arrival.flac")


def test_artist_evidence_artist_album_folder():
    assert artist_evidence(
        "Yan Qing", "@@abc\\Yan Qing - the arrival (2026)\\01. the arrival.flac"
    )


def test_artist_evidence_nested_share_layout_artist_as_grandparent():
    # The Artist/Album share layout: the artist is a directory level, not the parent
    # folder name - only a full-path scan carries it.
    assert artist_evidence(
        "Yan Qing", "@@hcbuf\\Music\\Yan Qing\\the arrival\\01 - the arrival.flac"
    )


def test_artist_evidence_accent_folded():
    assert artist_evidence("Sigur Rós", "@@x\\Sigur Ros\\( )\\01 untitled.flac")


def test_artist_evidence_majority_of_distinctive_words():
    # {florence, machine} after stopword filtering; one of two suffices (majority rule).
    assert artist_evidence(
        "Florence and the Machine", "@@x\\Florence + The Machine\\Lungs\\01.flac"
    )
    assert artist_evidence("Florence and the Machine", "@@x\\florence\\Lungs\\01.flac")


def test_artist_evidence_stopword_artist_needs_distinctive_word():
    # "The Who" must not count the ubiquitous "the" as evidence.
    assert not artist_evidence("The Who", "@@x\\the arrival\\the arrival.flac")
    assert artist_evidence("The Who", "@@x\\The Who\\Tommy\\01 Overture.flac")


def test_artist_evidence_all_stopword_artist_falls_back_to_full_name():
    # "The The" is entirely stopwords - fall back to the raw words rather than
    # having no evidence path at all.
    assert artist_evidence("The The", "@@x\\The The\\Soul Mining\\01.flac")


def test_fedition03_valid_reissue_descriptors_are_not_wrong_album() -> None:
    """F-EDITION-03: signed descriptor additions (OKNOTOK, MFSL, Immersion,
    Half Speed Master, Audiophile) are harmless edition words - a self-titled
    candidate carrying them must NOT read as a different album."""
    for candidate in (
        "Led Zeppelin (OKNOTOK)",
        "Led Zeppelin (MFSL)",
        "Led Zeppelin - Immersion.Box.Set",
        "Led Zeppelin Half Speed Master",
        "Led Zeppelin Audiophile",
        "Led Zeppelin Super Deluxe Experience",
    ):
        assert names_different_album(
            "Led Zeppelin", "Led Zeppelin", candidate
        ) is False, candidate


def test_fedition03_real_different_albums_still_reject() -> None:
    for candidate in ("Led Zeppelin - Kid A", "Led Zeppelin - Physical Graffiti"):
        assert names_different_album(
            "Led Zeppelin", "Led Zeppelin", candidate
        ) is True, candidate


def test_fedition03_sequel_numbering_still_rejects() -> None:
    assert names_different_album(
        "Led Zeppelin", "Led Zeppelin", "Led Zeppelin II"
    ) is True
    assert names_different_album(
        "Led Zeppelin", "Led Zeppelin", "Led Zeppelin III"
    ) is True


def test_boxset_compound_folds_but_bare_set_stays_an_album_word() -> None:
    # box + set folds to the canonical boxset descriptor on both sides.
    assert names_different_album(
        "Led Zeppelin Box Set", "Led Zeppelin", "Led Zeppelin (Box-Set)"
    ) is False
    # A bare "set" remains an album word: "Set" alone IS a different album.
    assert names_different_album(
        "Led Zeppelin", "Led Zeppelin", "Led Zeppelin - Set"
    ) is True


# GH-284: digit-bearing artist names earn evidence safely


@pytest.mark.parametrize(
    ("artist", "path"),
    [
        ("deadmau5", "/music/deadmau5/4x4=12/track-1.mp3"),
        ("deadmau5", "/music/Deadmau5 - For Lack of a Better Name/track-1.mp3"),
        ("U2", "/music/u2/the-joshua-tree/track-1.mp3"),
        ("311", "/music/311/greatest-hits/track-1.flac"),
        ("Matchbox 20", "/music/matchbox 20/yourself or someone like you/track-1.mp3"),
    ],
)
def test_digit_bearing_artists_match_their_own_paths(artist: str, path: str):
    assert artist_evidence(artist, path) is True


@pytest.mark.parametrize(
    ("artist", "path"),
    [
        # a bare year in the path is not the artist "311"
        ("311", "/music/various/greatest hits of 1994/311 - song.mp3".replace("311 - ", "")),
        # track ordinal containing the digits is not artist evidence
        ("311", "/music/various/album 311 disc/track-7.mp3"),
        # unrelated numeric directory
        ("311", "/music/1999/backstreet boys/track-1.mp3"),
        # wrong artist entirely
        ("deadmau5", "/music/other artist/album/track-1.mp3"),
        # obfuscated path without the name
        ("u2", "/music/a---b/c--d/track-1.mp3"),
    ],
)
def test_numeric_looking_paths_stay_negative(artist: str, path: str):
    assert artist_evidence(artist, path) is False


# GH #259 (scorer leg) + GH #307: canonical MusicBrainz punctuation vs scene-named releases.
# ``_SEP_RE`` used to split on ' " , ( ) so the canonical title "Man's Best Friend" tokenised
# to man + s (the 1-char fragment dropped), while the scene-named release drops the
# apostrophe entirely -> 'mans' read as a foreign word -> WRONG_ALBUM/PERMANENT. Same
# asymmetric-token mechanism for "O(verly) D(edicated)": o + verly, 'o' dropped, 'verly'
# foreign. One normalization pass in ``_tokens`` (apostrophe-delete + paren-unfold) closes
# both - these are the characterization tests that failed before it.

_MBF_ARTIST = "Sabrina Carpenter"
_MBF = "Man's Best Friend"


def test_gh259_scene_named_release_without_apostrophe_passes():
    # psykix's `dropped_wrong_album=4`: every release the indexer returned was rejected.
    assert not names_different_album(
        _MBF, _MBF_ARTIST, "Sabrina_Carpenter-Mans_Best_Friend-CD-FLAC-2025-GROUP"
    )


def test_gh259_human_named_release_without_apostrophe_passes():
    assert not names_different_album(
        _MBF, _MBF_ARTIST, "Sabrina Carpenter - Mans Best Friend (2025) [MP3-320]"
    )


def test_gh259_part_counter_prefixed_release_without_apostrophe_passes():
    assert not names_different_album(
        _MBF,
        _MBF_ARTIST,
        '[002/95] "Sabrina_Carpenter-Mans_Best_Friend-2025.part001.rar"',
    )


def test_gh259_smart_quote_title_matches_apostropheless_release_both_directions():
    # MusicBrainz titles carry the typographic apostrophe; the scene name drops it.
    # Either side may be the canonical one - the treatment is symmetric.
    assert not names_different_album(
        "Man’s Best Friend", _MBF_ARTIST, "Sabrina_Carpenter-Mans_Best_Friend-CD-FLAC-2025-GROUP"
    )
    assert not names_different_album(
        "Man’s Best Friend", _MBF_ARTIST, "Sabrina Carpenter - Mans Best Friend (2025) [MP3-320]"
    )
    # And the release may KEEP the apostrophe (ASCII or typographic) - still the same album.
    assert not names_different_album(
        _MBF, _MBF_ARTIST, "Sabrina Carpenter - Man's Best Friend (2025) [MP3-320]"
    )
    assert not names_different_album(
        _MBF, _MBF_ARTIST, "Sabrina Carpenter - Man’s Best Friend (2025) [MP3-320]"
    )


def test_gh259_genuinely_different_album_by_same_artist_still_rejects():
    # Negative control: relaxation must not open a wrong-album floodgate. "Emails I
    # Can't Send" carries 'emails' + 'send' - foreign album words regardless of the
    # apostrophe in "Can't".
    assert names_different_album(
        _MBF, _MBF_ARTIST, "Sabrina Carpenter - Emails I Can’t Send"
    )
    assert names_different_album(
        _MBF, _MBF_ARTIST, "Sabrina_Carpenter-Short_n_Sweet-CD-FLAC-2024-GROUP"
    )


def test_gh307_paren_obfuscated_title_unfolds_to_the_canonical_words():
    # "O(verly) D(edicated)" -> overly + dedicated; the 1-char 'o' and 'd' fragments
    # the old splitter dropped used to leave 'verly'/'edicated' reading as foreign.
    assert not names_different_album(
        "Overly Dedicated", "ScHoolboy Q", "Schoolboy_Q-O(verly)_D(edicated)-2010"
    )
    assert not names_different_album(
        "Overly Dedicated", "ScHoolboy Q", "ScHoolboy Q - O(verly) D(edicated) (2010)"
    )
    # The canonical title with real parens elsewhere still matches its own release.
    assert not names_different_album(
        "Overly Dedicated", "ScHoolboy Q", "Schoolboy_Q-Overly_Dedicated-CD-FLAC-2010"
    )


def test_gh307_standalone_paren_groups_stay_separate_tokens():
    # Only the #307 abutting shape (``O(verly)``) unfolds; a paren group with spaces
    # around/inside it keeps today's split behaviour exactly. ``strip_featuring``
    # removes a ``(feat. X)`` credit BEFORE tokenising (it is not album identity), so
    # the standalone-pins below use edition/year shapes for the same boundary.
    assert _tokens("Album (2014)") == ["album", "2014"]
    assert _tokens("(2014)") == ["2014"]
    assert _tokens("Suffocate (feat. Poppy)") == ["suffocate"]
    assert _tokens("Album (Deluxe Version)") == ["album", "deluxe", "version"]
    assert _tokens("Album (Disc 2)") == ["album", "disc", "2"]
    # ...and the abutting shape unfolds into the glued word.
    assert _tokens("O(verly) D(edicated)") == ["overly", "dedicated"]
    assert _tokens("Mans_Best_Friend") == ["mans", "best", "friend"]
    assert _tokens("Can't Stop the Music") == ["cant", "stop", "the", "music"]


def test_framing_quotes_still_split_and_word_internal_quotes_fold():
    # A quote with a space on one side FRAMES the title - it must keep splitting (a
    # ``'Man's Best Friend'`` candidate would otherwise tokenise to ``'mans`` and
    # read as a foreign word). A quote between two word chars is part of the word
    # (``Man"s`` -> ``mans``), same as the apostrophe.
    assert _tokens("'Man's Best Friend'") == ["mans", "best", "friend"]
    assert _tokens('"Sabrina Carpenter - Mans Best Friend"') == [
        "sabrina", "carpenter", "mans", "best", "friend",
    ]
    assert _tokens('Man"s Best Friend') == ["mans", "best", "friend"]
    assert not names_different_album(
        _MBF, _MBF_ARTIST, '"Sabrina Carpenter - Mans Best Friend"'
    )
    assert not names_different_album(
        'Man"s Best Friend', _MBF_ARTIST, "Sabrina_Carpenter-Mans_Best_Friend-2025"
    )


def test_gh259_gh307_containment_score_does_not_collapse():
    # The identity metrics run alongside the guard; they must not read the scene-named
    # release as a different work once the tokens fold. Scored the way the scorers
    # score candidate TITLES (artist words ignored - ``album_preflight_scorer`` passes
    # ``ignore=artist_words``); the scene group name is the one tolerated extra word,
    # matching how a clean scene-named release of ANY album scores today.
    _artist = frozenset({"sabrina", "carpenter"})
    assert title_containment_score(
        _MBF, "Sabrina_Carpenter-Mans_Best_Friend-CD-FLAC-2025-GROUP", ignore=_artist
    ) >= 0.75
    assert title_containment_score(
        _MBF, "Sabrina Carpenter - Mans Best Friend (2025) [MP3-320]", ignore=_artist
    ) == 1.0
    assert title_containment_score(
        "Overly Dedicated", "Schoolboy_Q-O(verly)_D(edicated)-2010",
        ignore=frozenset({"schoolboy", "q"}),
    ) == 1.0
    # ...and a genuinely different album still scores low.
    assert title_containment_score(
        _MBF, "Sabrina Carpenter - Emails I Can’t Send", ignore=_artist
    ) < 0.5
