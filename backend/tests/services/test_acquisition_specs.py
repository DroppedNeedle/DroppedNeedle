"""Acquisition spec-core tests (ArrRebuild step 1).

The headline design win over Lidarr: every spec is a PURE function, so these run
with ZERO mocks - ``DecisionContext`` is constructed directly. ``build_context``
(the single I/O step) gets one small fake-store test. The scorer suites
(``test_album_preflight_scorer`` / ``test_newznab_release_scorer``) remain the
behaviour safety net; these pin the migrated rules at the unit level.
"""

import pytest

from models.download import TargetAlbum
from models.download_identity import soulseek_identity, usenet_identity
from services.native.acquisition import pipeline
from services.native.acquisition.context import DecisionContext, build_context
from services.native.acquisition.decision import (
    Accept,
    Candidate,
    Disposition,
    Reject,
    RejectCode,
    SpecPolicy,
)
from services.native.acquisition.specs.free_space import free_space
from services.native.acquisition.specs.max_size import max_size
from services.native.acquisition.specs.min_age import min_age
from services.native.acquisition.specs.password import password
from services.native.acquisition.specs.quality_range import quality_range
from services.native.acquisition.specs.quarantine import quarantine
from services.native.acquisition.specs.retention import retention
from services.native.acquisition.specs.sample import sample
from services.native.acquisition.specs.terms import ignored_terms, required_terms
from services.native.acquisition.specs.upgrade_floor import upgrade_floor
from services.native.acquisition.specs.wrong_album import wrong_album
from services.native.acquisition.specs.wrong_edition import wrong_edition

_TARGET = TargetAlbum(artist_name="Radiohead", album_title="OK Computer", year=1997, track_count=12)
_POLICY = SpecPolicy(quality_min="mp3_320", quality_max="lossless")
_EMPTY = DecisionContext()


# --- quarantine spec ---------------------------------------------------------

def test_quarantine_rejects_blocklisted_identity():
    ident = usenet_identity("Radiohead - OK Computer", 600_000_000)
    ctx = DecisionContext(quarantine_set=frozenset({("usenet", ident)}))
    cand = Candidate(source="usenet", identity=ident)
    decision = quarantine(cand, _TARGET, ctx, _POLICY)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.BLOCKLISTED
    assert decision.disposition is Disposition.PERMANENT


def test_quarantine_accepts_clean_identity():
    ctx = DecisionContext(quarantine_set=frozenset({("usenet", "other")}))
    cand = Candidate(source="usenet", identity="not-listed")
    assert isinstance(quarantine(cand, _TARGET, ctx, _POLICY), Accept)


def test_quarantine_ignores_empty_identity():
    # A Soulseek folder carries no single identity; an empty identity never matches even
    # if some "" key were present, so the folder-level pipeline pass is a guaranteed no-op.
    ctx = DecisionContext(quarantine_set=frozenset({("soulseek", "")}))
    cand = Candidate(source="soulseek", identity="")
    assert isinstance(quarantine(cand, _TARGET, ctx, _POLICY), Accept)


def test_quarantine_namespaced_by_source():
    # Same identity string, different source -> not a match.
    ident = soulseek_identity("alice", "x/01.flac")
    ctx = DecisionContext(quarantine_set=frozenset({("usenet", ident)}))
    cand = Candidate(source="soulseek", identity=ident)
    assert isinstance(quarantine(cand, _TARGET, ctx, _POLICY), Accept)


# --- quality_range spec ------------------------------------------------------

def test_quality_range_unknown_passes():
    # Usenet noisy titles: 'unknown' tier passes (import tag-match is the real truth).
    cand = Candidate(source="usenet", tier="unknown")
    assert isinstance(quality_range(cand, _TARGET, _EMPTY, _POLICY), Accept)


def test_quality_range_in_range_accepts():
    cand = Candidate(source="soulseek", tier="lossless")
    assert isinstance(quality_range(cand, _TARGET, _EMPTY, _POLICY), Accept)


def test_quality_range_below_floor_rejected():
    cand = Candidate(source="soulseek", tier="low")
    decision = quality_range(cand, _TARGET, _EMPTY, _POLICY)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.QUALITY_REJECTED


def test_quality_range_above_ceiling_rejected():
    cand = Candidate(source="soulseek", tier="lossless")
    policy = SpecPolicy(quality_min="mp3_320", quality_max="mp3_320")
    decision = quality_range(cand, _TARGET, _EMPTY, policy)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.QUALITY_REJECTED


# --- wrong_album spec --------------------------------------------------------

def test_wrong_album_rejects_different_album_same_artist():
    cand = Candidate(source="usenet", match_text="Radiohead - In Rainbows [FLAC]")
    decision = wrong_album(cand, _TARGET, _EMPTY, _POLICY)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.WRONG_ALBUM


def test_wrong_album_accepts_same_album():
    cand = Candidate(source="soulseek", match_text="Radiohead OK Computer 1997")
    assert isinstance(wrong_album(cand, _TARGET, _EMPTY, _POLICY), Accept)


def test_wrong_album_accepts_obfuscated_title():
    # No readable artist -> left to the indexer-match base + import tag-match (Q4).
    cand = Candidate(source="usenet", match_text="aHR0cHM6 scrambled xQ.part01.rar")
    assert isinstance(wrong_album(cand, _TARGET, _EMPTY, _POLICY), Accept)


# --- pipeline ordering -------------------------------------------------------

def test_pipeline_accepts_when_all_specs_pass():
    cand = Candidate(source="soulseek", match_text="Radiohead OK Computer", tier="lossless")
    assert isinstance(pipeline.run(cand, _TARGET, _EMPTY, _POLICY), Accept)


def test_pipeline_returns_first_reject_blocklist_before_quality():
    # A candidate that is BOTH blocklisted AND out-of-quality returns BLOCKLISTED:
    # quarantine runs before quality_range in the ordered list.
    ident = usenet_identity("bad", 1)
    ctx = DecisionContext(quarantine_set=frozenset({("usenet", ident)}))
    cand = Candidate(source="usenet", identity=ident, match_text="Radiohead - In Rainbows", tier="low")
    decision = pipeline.run(cand, _TARGET, ctx, _POLICY)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.BLOCKLISTED


def test_pipeline_wrong_album_before_quality():
    # wrong_album precedes quality_range: a different album with a bad tier reports
    # WRONG_ALBUM, not QUALITY_REJECTED.
    cand = Candidate(source="usenet", match_text="Radiohead - In Rainbows [MP3]", tier="low")
    decision = pipeline.run(cand, _TARGET, _EMPTY, _POLICY)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.WRONG_ALBUM


# --- password spec (step 2) --------------------------------------------------

def test_password_rejects_protected():
    decision = password(Candidate(source="usenet", password=1), _TARGET, _EMPTY, _POLICY)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.PASSWORD_PROTECTED


def test_password_accepts_unprotected():
    assert isinstance(password(Candidate(source="usenet"), _TARGET, _EMPTY, _POLICY), Accept)


# --- wrong_edition spec (step 2, M3) -----------------------------------------

_LZ = TargetAlbum(artist_name="Led Zeppelin", album_title="Led Zeppelin", year=1969, track_count=9)


def test_wrong_edition_rejects_live_for_studio():
    cand = Candidate(source="usenet", match_text="Led Zeppelin - Live EP (FLAC)")
    decision = wrong_edition(cand, _LZ, _EMPTY, _POLICY)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.WRONG_EDITION


def test_wrong_edition_underscore_separated_live():
    # ``_`` is a regex word char; the spec must flatten it or miss ``Led_Zeppelin-Live_EP``.
    cand = Candidate(source="soulseek", match_text="Led_Zeppelin-Live_EP-FLAC-1969")
    assert isinstance(wrong_edition(cand, _LZ, _EMPTY, _POLICY), Reject)


def test_wrong_edition_keeps_live_when_requested():
    target = TargetAlbum(artist_name="Led Zeppelin", album_title="How the West Was Won (Live)")
    cand = Candidate(source="usenet", match_text="Led Zeppelin - How the West Was Won (Live) [FLAC]")
    assert isinstance(wrong_edition(cand, target, _EMPTY, _POLICY), Accept)


def test_wrong_edition_accepts_plain_studio():
    cand = Candidate(source="soulseek", match_text="Radiohead OK Computer 1997")
    assert isinstance(wrong_edition(cand, _TARGET, _EMPTY, _POLICY), Accept)


def test_wrong_edition_allows_singular_complete_album_tag():
    # "[Complete Album]" is an uploader 'full album, not a teaser' tag, not a discography -
    # the singular must NOT match the "complete ... albums" plural discography pattern.
    cand = Candidate(source="soulseek", match_text="Radiohead - OK Computer [Complete Album]")
    assert isinstance(wrong_edition(cand, _TARGET, _EMPTY, _POLICY), Accept)


def test_wrong_edition_still_rejects_complete_albums_discography():
    target = TargetAlbum(artist_name="Pink Floyd", album_title="Animals")
    cand = Candidate(source="usenet", match_text="Pink Floyd - The Complete Albums Collection [FLAC]")
    assert isinstance(wrong_edition(cand, target, _EMPTY, _POLICY), Reject)


# --- sample spec (step 2) ----------------------------------------------------

def test_sample_rejects_sample_marker():
    cand = Candidate(source="usenet", match_text="Radiohead - OK Computer (Sample)")
    decision = sample(cand, _TARGET, _EMPTY, _POLICY)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.SAMPLE


def test_sample_ignores_sample_rate_descriptor():
    # "sample rate" is a quality descriptor, not a sample marker.
    cand = Candidate(source="usenet", match_text="Radiohead - OK Computer [24bit sample rate 96kHz]")
    assert isinstance(sample(cand, _TARGET, _EMPTY, _POLICY), Accept)


def test_sample_kept_when_requested_album_contains_sample():
    target = TargetAlbum(artist_name="X", album_title="Sample This")
    cand = Candidate(source="usenet", match_text="X - Sample This [FLAC]")
    assert isinstance(sample(cand, target, _EMPTY, _POLICY), Accept)


# --- terms specs (step 2) ----------------------------------------------------

def test_ignored_terms_substring():
    policy = SpecPolicy(ignored_terms=("bootleg",))
    cand = Candidate(source="usenet", match_text="Artist - Album (2012 Bootleg)")
    decision = ignored_terms(cand, _TARGET, _EMPTY, policy)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.IGNORED_TERM


def test_ignored_terms_regex():
    policy = SpecPolicy(ignored_terms=(r"/v0|v2/",))
    cand = Candidate(source="usenet", match_text="Artist - Album [MP3 V0]")
    assert isinstance(ignored_terms(cand, _TARGET, _EMPTY, policy), Reject)


def test_ignored_terms_empty_is_noop():
    cand = Candidate(source="usenet", match_text="anything")
    assert isinstance(ignored_terms(cand, _TARGET, _EMPTY, _POLICY), Accept)


def test_malformed_regex_term_never_crashes():
    policy = SpecPolicy(ignored_terms=("/[unclosed/",))
    cand = Candidate(source="usenet", match_text="Artist - Album")
    assert isinstance(ignored_terms(cand, _TARGET, _EMPTY, policy), Accept)


def test_required_terms_missing_rejected():
    policy = SpecPolicy(required_terms=("flac",))
    cand = Candidate(source="usenet", match_text="Artist - Album [MP3]")
    decision = required_terms(cand, _TARGET, _EMPTY, policy)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.REQUIRED_TERM_MISSING


def test_required_terms_present_accepted():
    policy = SpecPolicy(required_terms=("flac",))
    cand = Candidate(source="usenet", match_text="Artist - Album [FLAC]")
    assert isinstance(required_terms(cand, _TARGET, _EMPTY, policy), Accept)


def test_required_terms_empty_is_noop():
    cand = Candidate(source="usenet", match_text="Artist - Album")
    assert isinstance(required_terms(cand, _TARGET, _EMPTY, _POLICY), Accept)


# --- max_size spec (step 2) --------------------------------------------------

_MB = 1024 * 1024


def test_max_size_rejects_oversize():
    policy = SpecPolicy(max_size_mb=100)
    cand = Candidate(source="usenet", size_bytes=200 * _MB)
    decision = max_size(cand, _TARGET, _EMPTY, policy)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.MAX_SIZE_EXCEEDED


def test_max_size_accepts_under_cap():
    policy = SpecPolicy(max_size_mb=100)
    cand = Candidate(source="usenet", size_bytes=50 * _MB)
    assert isinstance(max_size(cand, _TARGET, _EMPTY, policy), Accept)


def test_max_size_zero_is_unbounded():
    cand = Candidate(source="usenet", size_bytes=10_000 * _MB)
    assert isinstance(max_size(cand, _TARGET, _EMPTY, _POLICY), Accept)


# --- retention + min_age specs (step 2, Usenet age) --------------------------

_NOW = 1_700_000_000.0
_TIMED = DecisionContext(now=_NOW)
_DAY = 86400.0


def test_retention_rejects_too_old():
    policy = SpecPolicy(usenet_retention_days=30)
    cand = Candidate(source="usenet", usenet_date=_NOW - 40 * _DAY)
    decision = retention(cand, _TARGET, _TIMED, policy)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.RETENTION_EXCEEDED


def test_retention_accepts_within_window():
    policy = SpecPolicy(usenet_retention_days=30)
    cand = Candidate(source="usenet", usenet_date=_NOW - 10 * _DAY)
    assert isinstance(retention(cand, _TARGET, _TIMED, policy), Accept)


def test_retention_off_and_undated_pass():
    fresh = Candidate(source="usenet", usenet_date=_NOW - 9999 * _DAY)
    assert isinstance(retention(fresh, _TARGET, _TIMED, _POLICY), Accept)  # days=0 -> off
    undated = Candidate(source="soulseek")
    assert isinstance(retention(undated, _TARGET, _TIMED, SpecPolicy(usenet_retention_days=30)), Accept)


def test_min_age_rejects_too_young_as_temporary():
    policy = SpecPolicy(usenet_min_age_minutes=30)
    cand = Candidate(source="usenet", usenet_date=_NOW - 10 * 60)
    decision = min_age(cand, _TARGET, _TIMED, policy)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.TOO_YOUNG
    assert decision.disposition is Disposition.TEMPORARY  # retry once propagated, never blocklist


def test_min_age_accepts_old_enough():
    policy = SpecPolicy(usenet_min_age_minutes=30)
    cand = Candidate(source="usenet", usenet_date=_NOW - 60 * 60)
    assert isinstance(min_age(cand, _TARGET, _TIMED, policy), Accept)


def test_min_age_off_by_default_and_undated_pass():
    young = Candidate(source="usenet", usenet_date=_NOW - 1)
    assert isinstance(min_age(young, _TARGET, _TIMED, _POLICY), Accept)  # minutes=0 -> off
    undated = Candidate(source="soulseek")
    assert isinstance(min_age(undated, _TARGET, _TIMED, SpecPolicy(usenet_min_age_minutes=30)), Accept)


# --- free_space spec (step 2) ------------------------------------------------

def test_free_space_unknown_passes():
    cand = Candidate(source="usenet", size_bytes=5_000 * _MB)
    assert isinstance(free_space(cand, _TARGET, _EMPTY, _POLICY), Accept)  # free_bytes None


def test_free_space_rejects_insufficient_as_local_fault():
    ctx = DecisionContext(free_bytes=50 * _MB)
    cand = Candidate(source="usenet", size_bytes=10 * _MB)  # 50-10 < 100MB margin
    decision = free_space(cand, _TARGET, ctx, _POLICY)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.INSUFFICIENT_SPACE
    assert decision.disposition is Disposition.LOCAL_FAULT  # our disk, never blame the source


def test_free_space_accepts_ample():
    ctx = DecisionContext(free_bytes=10_000 * _MB)
    cand = Candidate(source="usenet", size_bytes=1_000 * _MB)
    assert isinstance(free_space(cand, _TARGET, ctx, _POLICY), Accept)


# --- upgrade_floor spec (CollectionManagement D12) ----------------------------

def test_upgrade_floor_passes_when_not_an_upgrade_run():
    # held_tier None = not an upgrade run (or target not held): everything passes,
    # so a first acquisition is never wrongly rejected. This is also the reconcile
    # point with the origin-aware album gate: a satisfied album never reaches the
    # spec, and an unheld one arrives here with held_tier None.
    ctx = DecisionContext(held_tier=None)
    cand = Candidate(source="soulseek", tier="mp3_192")
    assert isinstance(upgrade_floor(cand, _TARGET, ctx, _POLICY), Accept)


def test_upgrade_floor_rejects_equal_and_worse():
    ctx = DecisionContext(held_tier="mp3_320")
    for tier in ("mp3_320", "mp3_256", "mp3_192", "low"):
        decision = upgrade_floor(Candidate(source="soulseek", tier=tier), _TARGET, ctx, _POLICY)
        assert isinstance(decision, Reject), tier
        assert decision.code is RejectCode.NOT_AN_UPGRADE
        assert decision.disposition is Disposition.PERMANENT


def test_upgrade_floor_accepts_strictly_better():
    ctx = DecisionContext(held_tier="mp3_320")
    cand = Candidate(source="soulseek", tier="lossless")
    assert isinstance(upgrade_floor(cand, _TARGET, ctx, _POLICY), Accept)


def test_upgrade_floor_unknown_tier_defers_to_import_guard():
    # Usenet titles can hide the codec; the strictly-better check re-runs at
    # placement, so an unknown tier passes here (mirrors quality_range).
    ctx = DecisionContext(held_tier="mp3_320")
    for tier in ("unknown", ""):
        cand = Candidate(source="usenet", tier=tier)
        assert isinstance(upgrade_floor(cand, _TARGET, ctx, _POLICY), Accept), tier


def test_upgrade_floor_registered_in_pipeline():
    # A held mp3_320 album: an equal-tier candidate must be dropped by the shared
    # pipeline (not just the standalone spec).
    ctx = DecisionContext(held_tier="mp3_320")
    decision = pipeline.run(
        Candidate(source="soulseek", tier="mp3_320", match_text="Radiohead - OK Computer"),
        _TARGET, ctx, _POLICY,
    )
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.NOT_AN_UPGRADE


# --- build_context (the single I/O step) -------------------------------------

@pytest.mark.asyncio
async def test_build_context_snapshots_quarantine_set():
    class _FakeStore:
        async def load_quarantine_set(self):
            return {("usenet", "id-1"), ("soulseek", "id-2")}

    ctx = await build_context(_FakeStore())
    assert isinstance(ctx.quarantine_set, frozenset)
    assert ("usenet", "id-1") in ctx.quarantine_set
    assert ("soulseek", "id-2") in ctx.quarantine_set
