"""ST2 P1: MbCanonicalStore unit tests - real SQLite at tmp_path.

Covers: construct-twice-on-same-path idempotency, release_to_rg batch
read/write with '' negative, canonical_redirect identity-lane gate,
ISRC banking, seed migration from mbid_resolution_map.
"""

import sqlite3

import pytest

from infrastructure.persistence.mb_canonical_store import (
    OFFICIAL_MB_API_BASE,
    MbCanonicalStore,
)
from repositories.musicbrainz_base import (
    MB_TRUSTED_IDENTITY_ORIGINS,
    MbSourceContext,
    capture_mb_source_context,
    is_mb_identity_source,
    is_mb_rate_policy_public_host,
    normalize_mb_source_label,
    set_mb_api_base,
)

_EMPTY_SOURCE_CONTEXT = MbSourceContext(
    source_url="", generation=0, source_mode="", source_id=""
)
_LEGACY_SOURCE_CONTEXT = MbSourceContext(
    source_url="", generation=0, source_mode="legacy", source_id=""
)
_OFFICIAL_SOURCE_CONTEXT = MbSourceContext(
    source_url=OFFICIAL_MB_API_BASE, generation=0, source_mode="official", source_id=""
)


@pytest.fixture
def db_path(tmp_path):
    return tmp_path / "library.db"


@pytest.fixture
def write_lock():
    import threading

    return threading.Lock()


@pytest.fixture
def store(db_path, write_lock):
    return MbCanonicalStore(db_path=db_path, write_lock=write_lock)


class TestConstructTwiceIdempotency:
    def test_construct_twice_on_same_path(self, db_path, write_lock):
        MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        # Second construction must not raise (tables already exist).
        store2 = MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        assert store2.db_path == store2.db_path

    def test_tables_exist_after_construction(self, db_path, write_lock):
        MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        conn = sqlite3.connect(str(db_path))
        tables = {
            row[0]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table'"
            ).fetchall()
        }
        conn.close()
        assert "canonical_redirect" in tables
        assert "release_to_rg" in tables
        assert "recording_isrc" in tables


class TestSourceHostRatchet:
    @pytest.mark.asyncio
    async def test_source_labels_are_sanitized_idempotently(self, db_path, write_lock):
        MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        conn = sqlite3.connect(str(db_path))
        conn.execute(
            """
            INSERT INTO canonical_redirect (
                entity_kind, from_mbid_lower, to_mbid_lower, source,
                source_host, first_seen_at, last_confirmed_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            (
                "recording",
                "raw-recording",
                "raw-target",
                "test",
                "https://user:password@musicbrainz.org/ws/2?secret=1",
                1,
                1,
            ),
        )
        conn.execute(
            """
            INSERT INTO release_to_rg (
                release_mbid_lower, rg_mbid, source, source_host, saved_at
            ) VALUES (?, ?, ?, ?, ?)
            """,
            (
                "raw-release",
                "raw-rg",
                "test",
                "http://user:password@musicbrainz.org:8080/ws/2/path?secret=1#fragment",
                1,
            ),
        )
        conn.execute(
            """
            INSERT INTO canonical_redirect (
                entity_kind, from_mbid_lower, to_mbid_lower, source,
                source_host, first_seen_at, last_confirmed_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            ("recording", "legacy-empty", "legacy-target", "legacy", "", 1, 1),
        )
        conn.commit()
        conn.close()

        store2 = MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        conn = sqlite3.connect(str(db_path))
        canonical_row = conn.execute(
            "SELECT source_host, official_evidence FROM canonical_redirect "
            "WHERE from_mbid_lower = ?",
            ("raw-recording",),
        ).fetchone()
        release_row = conn.execute(
            "SELECT source_host, official_evidence FROM release_to_rg "
            "WHERE release_mbid_lower = ?",
            ("raw-release",),
        ).fetchone()
        empty_host = conn.execute(
            "SELECT source_host FROM canonical_redirect WHERE from_mbid_lower = ?",
            ("legacy-empty",),
        ).fetchone()[0]
        conn.close()

        assert canonical_row == ("", 1)
        assert release_row == ("", 0)
        assert empty_host == ""
        assert await store2.get_canonical_redirect(
            "recording",
            ["raw-recording"],
            source_context=_EMPTY_SOURCE_CONTEXT,
            trusted_identity_source_only=True,
        ) == {"raw-recording": "raw-target"}
        store3 = MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        conn = sqlite3.connect(str(db_path))
        canonical_row_again = conn.execute(
            "SELECT source_host, official_evidence FROM canonical_redirect "
            "WHERE from_mbid_lower = ?",
            ("raw-recording",),
        ).fetchone()
        release_row_again = conn.execute(
            "SELECT source_host, official_evidence FROM release_to_rg "
            "WHERE release_mbid_lower = ?",
            ("raw-release",),
        ).fetchone()
        conn.close()
        assert canonical_row_again == canonical_row
        assert release_row_again == release_row
        assert store3.db_path == db_path

    def test_ratchet_uses_full_canonical_redirect_key(self, db_path, write_lock):
        MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        with sqlite3.connect(str(db_path)) as conn:
            conn.executemany(
                """
                INSERT INTO canonical_redirect (
                    entity_kind, from_mbid_lower, to_mbid_lower, source,
                    source_host, first_seen_at, last_confirmed_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                """,
                [
                    (
                        "recording",
                        "same",
                        "recording-target",
                        "test",
                        "https://user:password@musicbrainz.org/ws/2?secret=1",
                        1,
                        1,
                    ),
                    (
                        "release",
                        "same",
                        "release-target",
                        "test",
                        "https://mirror.example/ws/2?secret=2",
                        1,
                        1,
                    ),
                ],
            )

        MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        with sqlite3.connect(str(db_path)) as conn:
            rows = {
                row[0]: tuple(row[1:])
                for row in conn.execute(
                    "SELECT entity_kind, source_host, official_evidence "
                    "FROM canonical_redirect "
                    "WHERE from_mbid_lower = ? ORDER BY entity_kind",
                    ("same",),
                ).fetchall()
            }
        assert rows == {
            "recording": ("", 1),
            "release": ("", 0),
        }

        MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        with sqlite3.connect(str(db_path)) as conn:
            rows_again = {
                row[0]: tuple(row[1:])
                for row in conn.execute(
                    "SELECT entity_kind, source_host, official_evidence "
                    "FROM canonical_redirect "
                    "WHERE from_mbid_lower = ? ORDER BY entity_kind",
                    ("same",),
                ).fetchall()
            }
        assert rows_again == rows

    def test_rate_policy_and_identity_predicates_are_separate(self):
        assert is_mb_rate_policy_public_host("https://musicbrainz.org/ws/2") is True
        assert is_mb_rate_policy_public_host("http://musicbrainz.org/ws/2") is True
        assert is_mb_rate_policy_public_host("http://musicbrainz.org:80/ws/2") is True
        assert (
            is_mb_rate_policy_public_host("https://musicbrainz.org:8443/ws/2") is False
        )

        assert is_mb_identity_source("https://musicbrainz.org/ws/2") is True
        assert is_mb_identity_source("https://musicbrainz.org:443/ws/2") is True
        assert is_mb_identity_source("http://musicbrainz.org/ws/2") is False
        assert is_mb_identity_source("http://musicbrainz.org:80/ws/2") is False
        assert MB_TRUSTED_IDENTITY_ORIGINS == tuple(sorted(MB_TRUSTED_IDENTITY_ORIGINS))
        assert all(
            is_mb_identity_source(origin) for origin in MB_TRUSTED_IDENTITY_ORIGINS
        )
        assert is_mb_identity_source("https://musicbrainz.org:8443/ws/2") is False

    @pytest.mark.asyncio
    async def test_new_source_labels_strip_url_detail(self, store):
        raw_source = "https://user:password@mirror.example:8443/ws/2?token=secret"
        await store.save_release_to_rg({"new-release": "new-rg"}, raw_source)
        await store.save_canonical_redirect(
            [
                {
                    "entity_kind": "recording",
                    "from_mbid": "new-recording",
                    "to_mbid": "new-target",
                }
            ],
            raw_source,
        )
        conn = sqlite3.connect(str(store.db_path))
        release_row = conn.execute(
            "SELECT source_host, official_evidence FROM release_to_rg "
            "WHERE release_mbid_lower = ?",
            ("new-release",),
        ).fetchone()
        redirect_row = conn.execute(
            "SELECT source_host, official_evidence FROM canonical_redirect "
            "WHERE from_mbid_lower = ?",
            ("new-recording",),
        ).fetchone()
        conn.close()
        assert release_row == ("", 0)
        assert redirect_row == ("", 0)

    def test_normalize_source_label_keeps_only_origin(self):
        assert (
            normalize_mb_source_label(
                "https://user:password@MUSICBRAINZ.ORG:443/ws/2?q=secret#fragment"
            )
            == "https://musicbrainz.org:443"
        )


@pytest.mark.asyncio
async def test_official_identity_gate_accepts_explicit_https_default_port_only(store):
    await store.save_canonical_redirect(
        [{"entity_kind": "recording", "from_mbid": "tls-443", "to_mbid": "target-443"}],
        "https://musicbrainz.org:443/ws/2",
    )
    await store.save_canonical_redirect(
        [
            {
                "entity_kind": "recording",
                "from_mbid": "tls-custom",
                "to_mbid": "target-custom",
            }
        ],
        "https://musicbrainz.org:8443/ws/2",
    )
    await store.save_canonical_redirect(
        [
            {
                "entity_kind": "recording",
                "from_mbid": "http-official",
                "to_mbid": "target-http",
            }
        ],
        "http://musicbrainz.org/ws/2",
    )

    result = await store.get_canonical_redirect(
        "recording",
        ["tls-443", "tls-custom", "http-official"],
        source_context=_OFFICIAL_SOURCE_CONTEXT,
        trusted_identity_source_only=True,
    )

    assert result == {"tls-443": "target-443"}


class TestReleaseToRg:
    @pytest.mark.asyncio
    async def test_save_and_batch_read(self, store):
        await store.save_release_to_rg(
            {"rel-1": "rg-1", "rel-2": ""}, source_host="https://mb.example"
        )
        result = await store.get_release_to_rg_batch(
            ["rel-1", "rel-2"], source_context=_LEGACY_SOURCE_CONTEXT
        )
        assert result["rel-1"] == "rg-1"
        assert result["rel-2"] == ""  # authoritative negative

    @pytest.mark.asyncio
    async def test_foreign_generation_rows_do_not_satisfy_current_lookup(self, store):
        with sqlite3.connect(str(store.db_path)) as conn:
            conn.execute(
                """
                INSERT INTO release_to_rg (
                    release_mbid_lower, rg_mbid, source, source_host,
                    source_mode, source_id, source_generation, saved_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    "rel-foreign",
                    "rg-foreign",
                    "test",
                    "",
                    "mirror",
                    "mirror-source",
                    3,
                    1,
                ),
            )
        current = MbSourceContext(
            source_url="https://mirror.example/ws/2",
            generation=4,
            source_mode="mirror",
            source_id="mirror-source",
        )
        assert (
            await store.get_release_to_rg_batch(["rel-foreign"], source_context=current)
            == {}
        )

    @pytest.mark.asyncio
    async def test_empty_string_is_authoritative_negative(self, store):
        await store.save_release_to_rg({"rel-neg": ""}, source_host="https://x")
        result = await store.get_release_to_rg_batch(
            ["rel-neg"], source_context=_LEGACY_SOURCE_CONTEXT
        )
        assert "rel-neg" in result  # present = known
        assert result["rel-neg"] == ""

    @pytest.mark.asyncio
    async def test_miss_returns_empty_dict(self, store):
        result = await store.get_release_to_rg_batch(
            ["never-seen"], source_context=_LEGACY_SOURCE_CONTEXT
        )
        assert result == {}


class TestCanonicalRedirect:
    @pytest.mark.asyncio
    async def test_save_and_read_official_only(self, store):
        rows = [
            {"entity_kind": "recording", "from_mbid": "old-1", "to_mbid": "new-1"},
        ]
        await store.save_canonical_redirect(rows, OFFICIAL_MB_API_BASE)

        result = await store.get_canonical_redirect(
            "recording",
            ["old-1"],
            source_context=_OFFICIAL_SOURCE_CONTEXT,
            trusted_identity_source_only=True,
        )
        assert result["old-1"] == "new-1"

    @pytest.mark.asyncio
    async def test_official_gate_filters_non_official_rows(self, store):
        rows = [
            {
                "entity_kind": "recording",
                "from_mbid": "old-mirror",
                "to_mbid": "new-mirror",
            },
        ]
        await store.save_canonical_redirect(rows, "https://hostile.example/ws/2")

        # Identity lane (official only) cannot see it.
        identity = await store.get_canonical_redirect(
            "recording",
            ["old-mirror"],
            source_context=_LEGACY_SOURCE_CONTEXT,
            trusted_identity_source_only=True,
        )
        assert "old-mirror" not in identity

        # Display lane CAN see it.
        display = await store.get_canonical_redirect(
            "recording",
            ["old-mirror"],
            source_context=_LEGACY_SOURCE_CONTEXT,
            trusted_identity_source_only=False,
        )
        assert display["old-mirror"] == "new-mirror"

    @pytest.mark.asyncio
    async def test_brainzmash_cannot_replace_official_identity_evidence(self, store):
        before = capture_mb_source_context()
        official = MbSourceContext(
            "https://musicbrainz.org/ws/2",
            before.generation + 1,
            source_mode="official",
            source_id="official-evidence-test",
        )
        brainzmash = MbSourceContext(
            "https://api.brainzmash.cc/ws/2",
            official.generation + 1,
            source_mode="brainzmash",
            source_id="brainzmash-evidence-test",
        )
        set_mb_api_base(
            official.source_url,
            source_mode=official.source_mode,
            source_id=official.source_id,
            generation=official.generation,
        )
        try:
            await store.save_canonical_redirect(
                [
                    {
                        "entity_kind": "recording",
                        "from_mbid": "same",
                        "to_mbid": "official",
                    }
                ],
                source_context=official,
            )
            set_mb_api_base(
                brainzmash.source_url,
                source_mode=brainzmash.source_mode,
                source_id=brainzmash.source_id,
                generation=brainzmash.generation,
            )
            await store.save_canonical_redirect(
                [
                    {
                        "entity_kind": "recording",
                        "from_mbid": "same",
                        "to_mbid": "brainz",
                    }
                ],
                source_context=brainzmash,
            )
        finally:
            set_mb_api_base(
                before.source_url,
                source_mode=before.source_mode,
                source_id=before.source_id,
                generation=before.generation,
            )

        with sqlite3.connect(str(store.db_path)) as conn:
            row = conn.execute(
                "SELECT to_mbid_lower, source_host, source_mode, source_id, "
                "source_generation, official_evidence "
                "FROM canonical_redirect WHERE from_mbid_lower = ?",
                ("same",),
            ).fetchone()
        assert row == (
            "official",
            "",
            "official",
            "official-evidence-test",
            official.generation,
            1,
        )


class TestIsrc:
    @pytest.mark.asyncio
    async def test_bank_and_retrieve(self, store):
        await store.save_isrc_recordings([("USUM71703861", "rec-abc")])
        recordings = await store.get_recordings_by_isrc(
            "USUM71703861", source_context=_EMPTY_SOURCE_CONTEXT
        )
        assert "rec-abc" in recordings

    @pytest.mark.asyncio
    async def test_unknown_isrc_returns_empty(self, store):
        assert (
            await store.get_recordings_by_isrc(
                "UNKNOWN", source_context=_EMPTY_SOURCE_CONTEXT
            )
            == []
        )


class TestSeedMigration:
    def _seed_legacy_table(self, db_path: str) -> None:
        conn = sqlite3.connect(db_path)
        conn.execute(
            """
            CREATE TABLE IF NOT EXISTS mbid_resolution_map (
                source_mbid_lower TEXT PRIMARY KEY,
                source_mbid TEXT NOT NULL,
                release_group_mbid TEXT
            )
            """
        )
        conn.execute(
            "INSERT INTO mbid_resolution_map VALUES "
            "('legacy-rel-lower', 'Legacy-Rel', 'legacy-rg'), "
            "('null-rg-lower', 'Null RG', NULL)"
        )
        conn.commit()
        conn.close()

    def test_seed_migration_populates_release_to_rg(self, db_path, write_lock):
        self._seed_legacy_table(str(db_path))
        store = MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        # Seed is synchronous inside __init__; verify via sync read on the DB.
        conn = sqlite3.connect(str(db_path))
        row = conn.execute(
            "SELECT rg_mbid FROM release_to_rg WHERE release_mbid_lower = ?",
            ("legacy-rel",),
        ).fetchone()
        conn.close()
        assert row is not None
        assert row[0] == "legacy-rg"

    def test_seed_migration_idempotent(self, db_path, write_lock):
        self._seed_legacy_table(str(db_path))
        MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        conn = sqlite3.connect(str(db_path))
        count = conn.execute(
            "SELECT COUNT(*) FROM release_to_rg WHERE release_mbid_lower = ?",
            ("legacy-rel",),
        ).fetchone()[0]
        conn.close()
        assert count == 1  # no duplicates from re-construction

    def test_null_rg_not_seeded_as_positive(self, db_path, write_lock):
        self._seed_legacy_table(str(db_path))
        MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        conn = sqlite3.connect(str(db_path))
        # NULL rg rows should NOT appear as positive entries
        row = conn.execute(
            "SELECT rg_mbid FROM release_to_rg WHERE release_mbid_lower = ?",
            ("null-rg",),
        ).fetchone()
        conn.close()
        # The seed query filters out NULLs so this row shouldn't exist
        assert row is None


@pytest.mark.asyncio
async def test_stale_source_context_rejects_all_canonical_writes(store):
    before = capture_mb_source_context()
    current = MbSourceContext(
        "https://mirror.example/ws/2",
        before.generation + 2,
        source_mode="mirror",
        source_id="current-source",
    )
    stale = MbSourceContext(
        "https://old.example/ws/2",
        before.generation + 1,
        source_mode="mirror",
        source_id="stale-source",
    )
    set_mb_api_base(
        current.source_url,
        source_mode=current.source_mode,
        source_id=current.source_id,
        generation=current.generation,
    )
    try:
        await store.save_release_to_rg(
            {"stale-release": "stale-rg"},
            source_context=stale,
        )
        await store.save_canonical_redirect(
            [{"entity_kind": "recording", "from_mbid": "stale", "to_mbid": "target"}],
            source_context=stale,
        )
        await store.save_isrc_recordings(
            [("US-STALE-1", "stale-recording")],
            source_context=stale,
        )
    finally:
        set_mb_api_base(
            before.source_url,
            source_mode=before.source_mode,
            source_id=before.source_id,
            generation=before.generation,
        )

    with sqlite3.connect(str(store.db_path)) as conn:
        assert (
            conn.execute(
                "SELECT COUNT(*) FROM release_to_rg WHERE release_mbid_lower = ?",
                ("stale-release",),
            ).fetchone()[0]
            == 0
        )
        assert (
            conn.execute(
                "SELECT COUNT(*) FROM canonical_redirect WHERE from_mbid_lower = ?",
                ("stale",),
            ).fetchone()[0]
            == 0
        )
        assert (
            conn.execute(
                "SELECT COUNT(*) FROM recording_isrc WHERE isrc = ?",
                ("US-STALE-1",),
            ).fetchone()[0]
            == 0
        )


class TestWriteThroughPersistence:
    @pytest.mark.asyncio
    async def test_failure_writes_nothing(self, store):
        """Transient failures must never write anything to the store."""
        initial_count = len(
            await store.get_release_to_rg_batch(
                [], source_context=_LEGACY_SOURCE_CONTEXT
            )
        )

        # save_release_to_rg with empty mapping writes nothing
        await store.save_release_to_rg({}, "https://x")
        result = await store.get_release_to_rg_batch(
            [], source_context=_LEGACY_SOURCE_CONTEXT
        )
        assert len(result) == initial_count
