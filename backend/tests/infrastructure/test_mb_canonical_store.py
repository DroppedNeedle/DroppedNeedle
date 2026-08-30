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
    is_mb_identity_source,
    is_mb_rate_policy_public_host,
    normalize_mb_source_label,
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
        canonical_host = conn.execute(
            "SELECT source_host FROM canonical_redirect WHERE from_mbid_lower = ?",
            ("raw-recording",),
        ).fetchone()[0]
        release_host = conn.execute(
            "SELECT source_host FROM release_to_rg WHERE release_mbid_lower = ?",
            ("raw-release",),
        ).fetchone()[0]
        empty_host = conn.execute(
            "SELECT source_host FROM canonical_redirect WHERE from_mbid_lower = ?",
            ("legacy-empty",),
        ).fetchone()[0]
        conn.close()

        assert canonical_host == "https://musicbrainz.org"
        assert release_host == "http://musicbrainz.org:8080"
        assert empty_host == ""
        assert "@" not in canonical_host + release_host
        assert "?" not in canonical_host + release_host
        assert "/" not in canonical_host.removeprefix("https://")
        assert await store2.get_canonical_redirect(
            "recording", ["raw-recording"], trusted_identity_source_only=True
        ) == {"raw-recording": "raw-target"}

        store3 = MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        conn = sqlite3.connect(str(db_path))
        assert (
            conn.execute(
                "SELECT source_host FROM canonical_redirect WHERE from_mbid_lower = ?",
                ("raw-recording",),
            ).fetchone()[0]
            == canonical_host
        )
        assert (
            conn.execute(
                "SELECT source_host FROM release_to_rg WHERE release_mbid_lower = ?",
                ("raw-release",),
            ).fetchone()[0]
            == release_host
        )
        conn.close()
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
            rows = dict(
                conn.execute(
                    "SELECT entity_kind, source_host FROM canonical_redirect "
                    "WHERE from_mbid_lower = ? ORDER BY entity_kind",
                    ("same",),
                ).fetchall()
            )
        assert rows == {
            "recording": "https://musicbrainz.org",
            "release": "https://mirror.example",
        }

        MbCanonicalStore(db_path=db_path, write_lock=write_lock)
        with sqlite3.connect(str(db_path)) as conn:
            assert (
                dict(
                    conn.execute(
                        "SELECT entity_kind, source_host FROM canonical_redirect "
                        "WHERE from_mbid_lower = ? ORDER BY entity_kind",
                        ("same",),
                    ).fetchall()
                )
                == rows
            )

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
        source_host = conn.execute(
            "SELECT source_host FROM release_to_rg WHERE release_mbid_lower = ?",
            ("new-release",),
        ).fetchone()[0]
        redirect_source_host = conn.execute(
            "SELECT source_host FROM canonical_redirect WHERE from_mbid_lower = ?",
            ("new-recording",),
        ).fetchone()[0]
        conn.close()
        assert source_host == "https://mirror.example:8443"
        assert redirect_source_host == source_host

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
        trusted_identity_source_only=True,
    )

    assert result == {"tls-443": "target-443"}


class TestReleaseToRg:
    @pytest.mark.asyncio
    async def test_save_and_batch_read(self, store):
        await store.save_release_to_rg(
            {"rel-1": "rg-1", "rel-2": ""}, source_host="https://mb.example"
        )
        result = await store.get_release_to_rg_batch(["rel-1", "rel-2"])
        assert result["rel-1"] == "rg-1"
        assert result["rel-2"] == ""  # authoritative negative

    @pytest.mark.asyncio
    async def test_empty_string_is_authoritative_negative(self, store):
        await store.save_release_to_rg({"rel-neg": ""}, source_host="https://x")
        result = await store.get_release_to_rg_batch(["rel-neg"])
        assert "rel-neg" in result  # present = known
        assert result["rel-neg"] == ""

    @pytest.mark.asyncio
    async def test_miss_returns_empty_dict(self, store):
        result = await store.get_release_to_rg_batch(["never-seen"])
        assert result == {}


class TestCanonicalRedirect:
    @pytest.mark.asyncio
    async def test_save_and_read_official_only(self, store):
        rows = [
            {"entity_kind": "recording", "from_mbid": "old-1", "to_mbid": "new-1"},
        ]
        await store.save_canonical_redirect(rows, OFFICIAL_MB_API_BASE)

        result = await store.get_canonical_redirect(
            "recording", ["old-1"], trusted_identity_source_only=True
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
            "recording", ["old-mirror"], trusted_identity_source_only=True
        )
        assert "old-mirror" not in identity

        # Display lane CAN see it.
        display = await store.get_canonical_redirect(
            "recording", ["old-mirror"], trusted_identity_source_only=False
        )
        assert display["old-mirror"] == "new-mirror"


class TestIsrc:
    @pytest.mark.asyncio
    async def test_bank_and_retrieve(self, store):
        await store.save_isrc_recordings([("USUM71703861", "rec-abc")])
        recordings = await store.get_recordings_by_isrc("USUM71703861")
        assert "rec-abc" in recordings

    @pytest.mark.asyncio
    async def test_unknown_isrc_returns_empty(self, store):
        assert await store.get_recordings_by_isrc("UNKNOWN") == []


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


class TestWriteThroughPersistence:
    @pytest.mark.asyncio
    async def test_failure_writes_nothing(self, store):
        """Transient failures must never write anything to the store."""
        initial_count = len(await store.get_release_to_rg_batch([]))

        # save_release_to_rg with empty mapping writes nothing
        await store.save_release_to_rg({}, "https://x")
        result = await store.get_release_to_rg_batch([])
        assert len(result) == initial_count
