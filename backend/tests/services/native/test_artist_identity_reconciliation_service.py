import hashlib
import sqlite3
import threading
from pathlib import Path
from unittest.mock import AsyncMock, Mock

import pytest

from core.exceptions import ExternalServiceError, StaleRevisionError
from infrastructure.persistence.native_library_store import (
    VARIOUS_ARTISTS_ID,
    NativeLibraryStore,
)
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalAlbumExternalIdentity,
    LocalArtist,
    LocalArtistCredit,
    LocalArtistExternalIdentity,
    LocalTrack,
    LocalTrackExternalIdentity,
)
from models.library_management import LibraryManagementMetadataSnapshot
from repositories.musicbrainz_management_models import (
    MbManagementArtist,
    MbManagementArtistCredit,
    MbManagementMedium,
    MbManagementRecording,
    MbManagementRelease,
    MbManagementReleaseGroup,
    MbManagementTrack,
)
from services.native.artist_identity_reconciliation_service import (
    _PROVIDER_DEFER_RETRY_SECONDS,
    ArtistIdentityReconciliationService,
)
from services.native.background_workload_gate import BackgroundWorkloadGate
from services.native.library_operation_supervisor import LibraryOperationSupervisor

RELEASE_GROUP_MBID = "dcff25f1-702d-3b5e-b0da-d48172e6e62a"
RELEASE_MBID = "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b"
RELEASE_TRACK_MBID = "22222222-2222-4222-8222-222222222222"
RECORDING_MBID = "33333333-3333-4333-8333-333333333333"
ARTIST_MBID = "7002bf88-1269-4965-a772-4ba1e7a91eaa"
OTHER_ARTIST_MBID = "24e1b53c-3085-33e9-8f3c-52404792e9a8"
VARIOUS_ARTISTS_MBID = "89ad4ac3-39f7-470e-963a-56509c546377"


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    path = tmp_path / "library.db"
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users VALUES ('admin')")
    return path


@pytest.fixture
def store(db_path: Path) -> NativeLibraryStore:
    return NativeLibraryStore(db_path, threading.Lock())


def _release(
    *, artist_mbid: str = ARTIST_MBID, artist_name: str = "Canonical Artist"
) -> MbManagementRelease:
    credit = MbManagementArtistCredit(
        name=artist_name,
        artist=MbManagementArtist(
            id=artist_mbid,
            name=artist_name,
            sort_name=artist_name,
        ),
    )
    return MbManagementRelease(
        id=RELEASE_MBID,
        title="Release",
        artist_credit=[credit],
        release_group=MbManagementReleaseGroup(
            id=RELEASE_GROUP_MBID,
            title="Release",
            artist_credit=[credit],
        ),
        media=[
            MbManagementMedium(
                position=1,
                tracks=[
                    MbManagementTrack(
                        id=RELEASE_TRACK_MBID,
                        title="Track",
                        position=1,
                        recording=MbManagementRecording(
                            id=RECORDING_MBID,
                            title="Track",
                            artist_credit=[credit],
                        ),
                    )
                ],
            )
        ],
    )


def _release_with_credits(
    credits: list[tuple[str, str, str, str]],
) -> MbManagementRelease:
    artist_credits = [
        MbManagementArtistCredit(
            name=credited_name,
            joinphrase=join_phrase,
            artist=MbManagementArtist(
                id=artist_mbid,
                name=canonical_name,
                sort_name=canonical_name,
            ),
        )
        for artist_mbid, canonical_name, credited_name, join_phrase in credits
    ]
    return MbManagementRelease(
        id=RELEASE_MBID,
        title="Release",
        artist_credit=artist_credits,
        release_group=MbManagementReleaseGroup(
            id=RELEASE_GROUP_MBID,
            title="Release",
            artist_credit=artist_credits,
        ),
        media=[
            MbManagementMedium(
                position=1,
                tracks=[
                    MbManagementTrack(
                        id=RELEASE_TRACK_MBID,
                        title="Track",
                        position=1,
                        artist_credit=artist_credits,
                        recording=MbManagementRecording(
                            id=RECORDING_MBID,
                            title="Track",
                            artist_credit=artist_credits,
                        ),
                    )
                ],
            )
        ],
    )


def _membership(suffix: str, artist_name: str) -> CatalogMembership:
    artist = LocalArtist(
        id=f"artist-{suffix}",
        display_name=artist_name,
        folded_name=artist_name.casefold(),
        normalized_name=artist_name.casefold(),
        kind="group",
        created_at=float(suffix),
        updated_at=float(suffix),
    )
    album = LocalAlbum(
        id=f"album-{suffix}",
        root_id="root-1",
        grouping_key=f"group-{suffix}",
        title="Release",
        album_artist_id=artist.id,
        album_artist_name=artist_name,
        created_at=1,
        updated_at=1,
    )
    track = LocalTrack(
        id=f"track-{suffix}",
        local_album_id=album.id,
        root_id="root-1",
        file_path=f"/music/{suffix}.flac",
        relative_path=f"{suffix}.flac",
        path_hash=f"hash-{suffix}",
        file_size_bytes=100,
        file_mtime_ns=200,
        stat_revision=f"stat-{suffix}",
        title="Track",
        artist_name=artist_name,
        album_title="Release",
        album_artist_name=artist_name,
        file_format="flac",
        imported_at=1,
        applied_policy="automatic",
    )
    credit = LocalArtistCredit(local_artist_id=artist.id, position=0)
    return CatalogMembership(
        album=album,
        artists=[artist],
        tracks=[track],
        album_credits=[credit],
        track_credits={track.id: [credit]},
    )


async def _accept_exact_identity(
    store: NativeLibraryStore, suffix: str, *, include_track: bool = True
) -> None:
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id=f"album-{suffix}",
            release_group_mbid=RELEASE_GROUP_MBID,
            release_mbid=RELEASE_MBID,
            selected_at=2,
        ),
        expected_album_revision=1,
    )
    if include_track:
        await store.attach_track_identity(
            LocalTrackExternalIdentity(
                local_track_id=f"track-{suffix}",
                recording_mbid=RECORDING_MBID,
                release_mbid=RELEASE_MBID,
                release_track_mbid=RELEASE_TRACK_MBID,
                medium_position=1,
                release_track_position=1,
                selected_at=2,
            ),
            expected_track_revision=1,
        )


def connection_state(
    db_path: Path, album_id: str
) -> tuple[str, str]:
    with sqlite3.connect(db_path) as connection:
        return connection.execute(
            "SELECT state, reason_code FROM library_artist_reconciliation_state "
            "WHERE local_album_id = ?",
            (album_id,),
        ).fetchone()


async def _run_album(
    service: ArtistIdentityReconciliationService,
    store: NativeLibraryStore,
    album_id: str,
) -> dict:
    job = await service.enqueue_album(album_id)
    assert job is not None
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    return await service.run_claimed(claimed, "worker")


async def _seed_provider_anchored_split(
    store: NativeLibraryStore,
    *,
    suffix: str = "split",
    embedded_artist_mbid: str | None = None,
) -> tuple[str, str, str]:
    survivor = LocalArtist(
        id=f"survivor-{suffix}",
        display_name="Same Artist",
        folded_name="same artist",
        normalized_name="same artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    source = LocalArtist(
        id=f"source-{suffix}",
        display_name="Same Artist",
        folded_name="same artist",
        normalized_name="same artist",
        kind="group",
        sort_name="Artist, Same",
        created_at=2,
        updated_at=2,
    )
    album = LocalAlbum(
        id=f"album-{suffix}",
        root_id="root-1",
        grouping_key=f"group-{suffix}",
        title="Release",
        album_artist_id=survivor.id,
        album_artist_name="Same Artist",
        created_at=1,
        updated_at=1,
    )
    track = LocalTrack(
        id=f"track-{suffix}",
        local_album_id=album.id,
        root_id="root-1",
        file_path=f"/music/{suffix}.flac",
        relative_path=f"{suffix}.flac",
        path_hash=f"hash-{suffix}",
        file_size_bytes=100,
        file_mtime_ns=200,
        stat_revision=f"stat-{suffix}",
        tag_revision=f"tag-{suffix}",
        title="Track",
        artist_name="Same Artist",
        album_title="Release",
        album_artist_name="Same Artist",
        embedded_artist_mbid=embedded_artist_mbid,
        file_format="flac",
        imported_at=1,
        applied_policy="automatic",
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[survivor, source],
            tracks=[track],
            album_credits=[
                LocalArtistCredit(
                    local_artist_id=survivor.id,
                    position=0,
                    credited_name="Same Artist",
                )
            ],
            track_credits={
                track.id: [
                    LocalArtistCredit(
                        local_artist_id=source.id,
                        position=0,
                        credited_name="Same Artist",
                    )
                ]
            },
        )
    )
    await store.attach_artist_identity_with_aliases(
        LocalArtistExternalIdentity(
            local_artist_id=survivor.id,
            provider_artist_id=ARTIST_MBID,
            decision_source="automatic",
            selected_at=2,
        ),
        [],
        expected_artist_revision=1,
    )
    return survivor.id, source.id, album.id


async def _add_provider_mismatched_album_track_credit(
    store: NativeLibraryStore,
    source_id: str,
    *,
    suffix: str,
) -> None:
    membership = _membership(suffix, "Different Artist")
    membership.tracks[0].artist_name = "Same Artist"
    await store.create_catalog_membership(
        CatalogMembership(
            album=membership.album,
            artists=membership.artists,
            tracks=membership.tracks,
            album_credits=membership.album_credits,
            track_credits={
                membership.tracks[0].id: [
                    LocalArtistCredit(
                        local_artist_id=source_id,
                        position=0,
                        credited_name="Same Artist",
                    )
                ]
            },
        )
    )
    await store.attach_artist_identity_with_aliases(
        LocalArtistExternalIdentity(
            local_artist_id=membership.artists[0].id,
            provider_artist_id=OTHER_ARTIST_MBID,
            decision_source="automatic",
            selected_at=2,
        ),
        [],
        expected_artist_revision=1,
    )


@pytest.mark.asyncio
async def test_different_names_merge_only_after_every_credit_has_same_provider_proof(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "First Local Name"))
    await store.create_catalog_membership(_membership("2", "Second Local Name"))
    await _accept_exact_identity(store, "1")
    await _accept_exact_identity(store, "2")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    on_catalog_changed = AsyncMock()
    service = ArtistIdentityReconciliationService(
        store, provider, on_catalog_changed=on_catalog_changed, clock=lambda: 3
    )

    await _run_album(service, store, "album-1")
    await _run_album(service, store, "album-2")

    with sqlite3.connect(db_path) as connection:
        owner = connection.execute(
            "SELECT local_artist_id FROM local_artist_external_identities "
            "WHERE provider = 'musicbrainz' AND provider_artist_id = ?",
            (ARTIST_MBID,),
        ).fetchone()[0]
        retired = dict(
            connection.execute(
                "SELECT id, retired_into_artist_id FROM local_artists "
                "WHERE id IN ('artist-1', 'artist-2')"
            ).fetchall()
        )
        aliases = dict(
            connection.execute(
                "SELECT alias, local_artist_id FROM local_artist_aliases "
                "WHERE alias IN ('artist-1', 'artist-2')"
            ).fetchall()
        )
        actions = connection.execute(
            "SELECT actor_user_id, reason_code FROM library_catalog_actions "
            "WHERE reason_code = 'AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE'"
        ).fetchall()
        proof_count = connection.execute(
            "SELECT COUNT(*) FROM library_artist_credit_proofs"
        ).fetchone()[0]
    assert owner not in {"artist-1", "artist-2"}
    assert retired == {"artist-1": owner, "artist-2": owner}
    assert aliases == {"artist-1": owner, "artist-2": owner}
    assert len(actions) == 2
    assert all(action[0] is None for action in actions)
    assert all(
        action[1] == "AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE"
        for action in actions
    )
    assert proof_count == 4
    assert provider.get_canonical_release.await_count == 2
    assert on_catalog_changed.await_count == 2


@pytest.mark.asyncio
async def test_current_dismissal_prevents_exact_provider_projection_retirement(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Canonical Artist"))
    await store.create_catalog_membership(_membership("2", "Canonical Artist"))
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    with sqlite3.connect(db_path) as connection:
        revisions = dict(
            connection.execute(
                "SELECT id, row_revision FROM local_artists "
                "WHERE id IN ('artist-1', 'artist-2')"
            ).fetchall()
        )
    await store.dismiss_artist_duplicate_group(
        artist_ids=["artist-1", "artist-2"],
        expected_revisions=revisions,
        actor_user_id="admin",
        now=3,
    )

    await _accept_exact_identity(store, "1")
    await _accept_exact_identity(store, "2")
    await _run_album(service, store, "album-1")
    await _run_album(service, store, "album-2")

    with sqlite3.connect(db_path) as connection:
        retired = dict(
            connection.execute(
                "SELECT id, retired_into_artist_id FROM local_artists "
                "WHERE id IN ('artist-1', 'artist-2')"
            ).fetchall()
        )
        action_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE reason_code = "
            "'AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE'"
        ).fetchone()[0]
    assert retired == {"artist-1": None, "artist-2": None}
    assert action_count == 0


@pytest.mark.asyncio
async def test_incomplete_track_mapping_projects_release_credit_but_does_not_merge_source(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Duplicate"))
    await store.create_catalog_membership(_membership("2", "Duplicate"))
    await _accept_exact_identity(store, "1")
    await _accept_exact_identity(store, "2", include_track=False)
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")
    await _run_album(service, store, "album-2")

    with sqlite3.connect(db_path) as connection:
        retired = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = 'artist-2'"
        ).fetchone()[0]
        state = connection.execute(
            "SELECT state FROM library_artist_reconciliation_state "
            "WHERE local_album_id = 'album-2'"
        ).fetchone()[0]
    assert retired is None
    assert state == "waiting_for_identity"


@pytest.mark.asyncio
async def test_no_embedded_ids_and_no_proof_do_not_retire_the_split_source(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-IDENT-01 option B: the name-anchored convergence path is proof-gated.

    No embedded artist MBIDs and no accepted release identity or stored proof
    rows means normalized-name agreement alone must not retire the source."""
    _survivor_id, source_id, album_id = await _seed_provider_anchored_split(store)
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    result = await _run_album(service, store, album_id)

    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (source_id,),
        ).fetchone()[0]
        track_artist_id = connection.execute(
            "SELECT local_artist_id FROM local_track_artists WHERE local_track_id = 'track-split'"
        ).fetchone()[0]
        action_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE reason_code = 'AUTOMATIC_PROVIDER_ANCHORED_ARTIST_CONVERGENCE'"
        ).fetchone()[0]
        aliases = connection.execute(
            "SELECT COUNT(*) FROM local_artist_aliases WHERE alias = 'source-split'"
        ).fetchone()[0]
    assert retired_into is None
    assert track_artist_id == source_id  # credits untouched
    assert action_count == 0
    assert aliases == 0
    # The proof-gated deferral reports waiting_for_identity, never the
    # convergence reason code.
    state = connection_state(db_path, album_id)
    assert state == ("waiting_for_identity", "EXACT_RELEASE_NOT_ACCEPTED")
    assert state[1] != "PROVIDER_ANCHORED_ARTISTS_CONVERGED"


@pytest.mark.asyncio
async def test_accepted_release_identity_proves_the_anchor_and_retires(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Album-level proof per F-IDENT-01 option B: an accepted MusicBrainz
    release identity on the source album authorizes the name-anchored
    retirement; references migrate through the surviving artist idempotently."""
    survivor_id, source_id, album_id = await _seed_provider_anchored_split(store)
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id=album_id,
            release_group_mbid=RELEASE_GROUP_MBID,
            release_mbid=RELEASE_MBID,
            selected_at=2,
        ),
        expected_album_revision=1,
    )

    await _run_album(service, store, album_id)
    merge_context = AsyncMock(wraps=store.get_artist_merge_context)
    store.get_artist_merge_context = merge_context
    listing = await service.list_groups(
        limit=50, cursor=None, state="resolved_automatically", search=None
    )
    progress = await service.progress()

    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (source_id,),
        ).fetchone()[0]
        track_artist_id = connection.execute(
            "SELECT local_artist_id FROM local_track_artists WHERE local_track_id = 'track-split'"
        ).fetchone()[0]
        action = connection.execute(
            "SELECT actor_user_id, reason_code, after_json FROM library_catalog_actions "
            "WHERE reason_code = 'AUTOMATIC_PROVIDER_ANCHORED_ARTIST_CONVERGENCE'"
        ).fetchone()
    assert retired_into == survivor_id
    assert track_artist_id == survivor_id
    assert action is not None
    assert action[0] is None
    assert action[1] == "AUTOMATIC_PROVIDER_ANCHORED_ARTIST_CONVERGENCE"
    assert ARTIST_MBID in action[2]
    assert listing.total == 1
    assert (
        listing.items[0].reason_code == "AUTOMATIC_PROVIDER_ANCHORED_ARTIST_CONVERGENCE"
    )
    assert progress.automatically_resolved_count == 1
    assert merge_context.await_count == 2

    # Idempotent: re-running the convergence transaction retires nothing
    # further and never duplicates the automatic action row.
    repeat_job = await service.enqueue_album(album_id)
    assert repeat_job is not None
    repeated = await store.apply_provider_anchored_artist_convergence(
        album_id,
        operation_job_id=repeat_job["id"],
        now=4,
    )
    assert repeated["retired_artist_ids"] == []
    with sqlite3.connect(db_path) as connection:
        action_total = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE reason_code = 'AUTOMATIC_PROVIDER_ANCHORED_ARTIST_CONVERGENCE'"
        ).fetchone()[0]
        alias_owner = connection.execute(
            "SELECT local_artist_id FROM local_artist_aliases WHERE alias = 'source-split'"
        ).fetchone()[0]
    assert action_total == 1
    assert alias_owner == survivor_id


@pytest.mark.asyncio
async def test_provider_anchor_never_overrides_conflicting_embedded_artist_identity(
    store: NativeLibraryStore, db_path: Path
) -> None:
    _survivor_id, source_id, album_id = await _seed_provider_anchored_split(
        store,
        suffix="conflict",
        embedded_artist_mbid=OTHER_ARTIST_MBID,
    )
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    await _run_album(service, store, album_id)

    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (source_id,),
        ).fetchone()[0]
        action_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE reason_code = "
            "'AUTOMATIC_PROVIDER_ANCHORED_ARTIST_CONVERGENCE'"
        ).fetchone()[0]
    assert retired_into is None
    assert action_count == 0


@pytest.mark.asyncio
async def test_embedded_provider_identity_resolves_unanchored_credits_of_same_artist(
    store: NativeLibraryStore, db_path: Path
) -> None:
    survivor_id, source_id, album_id = await _seed_provider_anchored_split(
        store,
        suffix="embedded",
        embedded_artist_mbid=ARTIST_MBID,
    )
    await _add_provider_mismatched_album_track_credit(
        store,
        source_id,
        suffix="4",
    )
    # Album-level proof (F-IDENT-01 option B): accepted release identity on the
    # source's own split album authorizes the convergence.
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id=album_id,
            release_group_mbid=RELEASE_GROUP_MBID,
            release_mbid=RELEASE_MBID,
            selected_at=2,
        ),
        expected_album_revision=1,
    )
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    await _run_album(service, store, album_id)

    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (source_id,),
        ).fetchone()[0]
        credited_artists = {
            row[0]
            for row in connection.execute(
                "SELECT local_artist_id FROM local_track_artists "
                "WHERE local_track_id IN ('track-embedded', 'track-4')"
            )
        }
    assert retired_into == survivor_id
    assert credited_artists == {survivor_id}


@pytest.mark.asyncio
async def test_album_anchor_does_not_claim_unproven_track_only_homonym(
    store: NativeLibraryStore, db_path: Path
) -> None:
    _survivor_id, source_id, album_id = await _seed_provider_anchored_split(
        store,
        suffix="unproven",
    )
    await _add_provider_mismatched_album_track_credit(
        store,
        source_id,
        suffix="5",
    )
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    await _run_album(service, store, album_id)

    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (source_id,),
        ).fetchone()[0]
    assert retired_into is None


@pytest.mark.asyncio
async def test_same_name_in_an_unrelated_album_is_not_provider_anchored(
    store: NativeLibraryStore, db_path: Path
) -> None:
    _survivor_id, source_id, album_id = await _seed_provider_anchored_split(store)
    unrelated = _membership("3", "Same Artist")
    unrelated = CatalogMembership(
        album=unrelated.album,
        artists=[],
        tracks=unrelated.tracks,
        album_credits=[
            LocalArtistCredit(
                local_artist_id=source_id, position=0, credited_name="Same Artist"
            )
        ],
        track_credits={
            "track-3": [
                LocalArtistCredit(
                    local_artist_id=source_id,
                    position=0,
                    credited_name="Same Artist",
                )
            ]
        },
    )
    unrelated.album.album_artist_id = source_id
    await store.create_catalog_membership(unrelated)
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    await _run_album(service, store, album_id)

    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (source_id,),
        ).fetchone()[0]
    assert retired_into is None


@pytest.mark.asyncio
async def test_same_name_with_conflicting_provider_identities_remains_separate(
    store: NativeLibraryStore,
) -> None:
    await store.create_catalog_membership(_membership("1", "Duplicate"))
    await store.create_catalog_membership(_membership("2", "Duplicate"))
    await _accept_exact_identity(store, "1")
    await _accept_exact_identity(store, "2")
    provider = AsyncMock()
    provider.get_canonical_release.side_effect = [
        _release(),
        _release(artist_mbid=OTHER_ARTIST_MBID),
    ]
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")
    await _run_album(service, store, "album-2")
    listing = await service.list_groups(limit=50, cursor=None, state=None, search=None)

    active = [item for item in listing.items if item.state != "resolved_automatically"]
    assert len(active) == 1
    assert active[0].state == "provider_conflict"
    assert set(active[0].provider_mbids) == {ARTIST_MBID, OTHER_ARTIST_MBID}


@pytest.mark.asyncio
async def test_name_match_without_proof_is_review_only_and_can_be_dismissed(
    store: NativeLibraryStore,
) -> None:
    await store.create_catalog_membership(_membership("1", "Duplicate"))
    await store.create_catalog_membership(_membership("2", "Duplicate"))
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    listing = await service.list_groups(limit=50, cursor=None, state=None, search=None)
    assert listing.total == 1
    group = listing.items[0]
    assert group.state == "same_name_only"

    result = await service.dismiss_group(
        group.id,
        {member.id: member.row_revision for member in group.members},
        "admin",
    )
    assert result.dismissed_pairs == 1
    assert (
        await service.list_groups(limit=50, cursor=None, state=None, search=None)
    ).total == 0


@pytest.mark.asyncio
async def test_provider_failure_defers_without_catalog_mutation(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Artist"))
    await _accept_exact_identity(store, "1")
    before = await store.get_catalog_revision()
    provider = AsyncMock()
    provider.get_canonical_release.side_effect = ExternalServiceError("unavailable")
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    job = await service.enqueue_album("album-1")
    assert job is not None
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    result = await service.run_claimed(claimed, "worker")

    assert result["state"] == "queued"
    assert await store.get_catalog_revision() == before
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_artist_credit_proofs"
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_catalog_actions WHERE reason_code = "
                "'AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE'"
            ).fetchone()[0]
            == 0
        )


@pytest.mark.asyncio
async def test_provider_failure_defer_retries_after_backoff_not_immediately(
    store: NativeLibraryStore,
) -> None:
    """The defer must not be instantly re-claimable: re-claiming the same item
    in a tight loop was the stuck-maintenance hot spin."""
    await store.create_catalog_membership(_membership("1", "Artist"))
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.side_effect = ExternalServiceError("unavailable")
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await service.enqueue_album("album-1")
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    deferred = await service.run_claimed(claimed, "worker")

    assert deferred["state"] == "queued"
    assert deferred["next_attempt_at"] == 3 + _PROVIDER_DEFER_RETRY_SECONDS
    assert (
        await store.claim_operation_job("worker", now=4, lease_seconds=60, kind="repair")
        is None
    )

    provider.get_canonical_release.side_effect = None
    provider.get_canonical_release.return_value = _release()
    reclaimed = await store.claim_operation_job(
        "worker",
        now=3 + _PROVIDER_DEFER_RETRY_SECONDS,
        lease_seconds=60,
        kind="repair",
    )
    assert reclaimed is not None
    assert reclaimed["next_attempt_at"] is None
    terminal = await service.run_claimed(reclaimed, "worker")
    assert terminal["state"] == "succeeded"


@pytest.mark.asyncio
async def test_scan_gate_defers_and_resumes_the_same_durable_work(
    store: NativeLibraryStore,
) -> None:
    await store.create_catalog_membership(_membership("1", "Artist"))
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    gate = BackgroundWorkloadGate()
    gate.set_scan_active(True)
    service = ArtistIdentityReconciliationService(
        store, provider, gate, clock=lambda: 3
    )

    await service.enqueue_album("album-1")
    claimed = await store.claim_operation_job(
        "worker-1", now=3, lease_seconds=60, kind="repair"
    )
    deferred = await service.run_claimed(claimed, "worker-1")
    assert deferred["state"] == "queued"
    provider.get_canonical_release.assert_not_awaited()

    gate.set_scan_active(False)
    resumed = await store.claim_operation_job(
        "worker-2", now=4, lease_seconds=60, kind="repair"
    )
    terminal = await service.run_claimed(resumed, "worker-2")
    assert terminal["state"] == "succeeded"
    provider.get_canonical_release.assert_awaited_once()


@pytest.mark.asyncio
async def test_expired_worker_lease_recovers_after_restart(
    store: NativeLibraryStore,
) -> None:
    await store.create_catalog_membership(_membership("1", "Artist"))
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 5)

    await service.enqueue_album("album-1")
    claimed = await store.claim_operation_job(
        "worker-before-restart", now=3, lease_seconds=1, kind="repair"
    )
    assert claimed is not None
    # (GH-293) Work rows are materialized in bounded pages when the worker runs;
    # a crash after a materialization page commit must resume without re-insert.
    staged = await store.materialize_repair_operation_batch(
        claimed["id"], "worker-before-restart", now=3
    )
    assert staged["complete"] is True
    running_work = await store.claim_operation_work(
        claimed["id"], "worker-before-restart", now=3
    )
    assert running_work is not None

    assert await store.recover_expired_operation_leases(now=5) == 1
    resumed = await store.claim_operation_job(
        "worker-after-restart", now=5, lease_seconds=60, kind="repair"
    )
    terminal = await service.run_claimed(resumed, "worker-after-restart")
    assert terminal["state"] == "succeeded"
    provider.get_canonical_release.assert_awaited_once()


@pytest.mark.asyncio
async def test_name_matching_provider_survivor_can_keep_unresolved_credits(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Local Alias"))
    await store.create_catalog_membership(_membership("2", "Canonical Artist"))
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")

    with sqlite3.connect(db_path) as connection:
        retired = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = 'artist-1'"
        ).fetchone()[0]
        owner = connection.execute(
            "SELECT local_artist_id FROM local_artist_external_identities "
            "WHERE provider_artist_id = ?",
            (ARTIST_MBID,),
        ).fetchone()[0]
        unresolved = connection.execute(
            "SELECT COUNT(*) FROM local_album_artists WHERE local_artist_id = 'artist-2'"
        ).fetchone()[0]
    assert retired == "artist-2"
    assert owner == "artist-2"
    assert unresolved == 2


@pytest.mark.asyncio
async def test_zero_track_album_credit_does_not_block_provider_proven_retirement(
    store: NativeLibraryStore, db_path: Path
) -> None:
    membership = _membership("1", "Local Alias")
    await store.create_catalog_membership(membership)
    orphan_album = LocalAlbum(
        id="album-orphan",
        root_id="root-1",
        grouping_key="group-orphan",
        title="Historical Empty Release",
        album_artist_id="artist-1",
        album_artist_name="Local Alias",
        created_at=1,
        updated_at=1,
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=orphan_album,
            artists=[],
            tracks=[],
            album_credits=[LocalArtistCredit(local_artist_id="artist-1", position=0)],
            track_credits={},
        )
    )
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")

    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = 'artist-1'"
        ).fetchone()[0]
        action_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE reason_code = "
            "'AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE'"
        ).fetchone()[0]
        orphan_count = connection.execute(
            "SELECT COUNT(*) FROM local_albums WHERE id = 'album-orphan'"
        ).fetchone()[0]
    assert retired_into is not None
    assert action_count == 1
    assert orphan_count == 1


@pytest.mark.asyncio
async def test_featured_credit_is_projected_but_ambiguous_composite_is_not_retired(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Main feat. Guest"))
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release_with_credits(
        [
            (ARTIST_MBID, "Main Artist", "Main", " feat. "),
            (OTHER_ARTIST_MBID, "Guest Artist", "Guest", ""),
        ]
    )
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")

    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        album = connection.execute(
            "SELECT album_artist_name FROM local_albums WHERE id = 'album-1'"
        ).fetchone()
        album_credits = connection.execute(
            "SELECT credited_name, join_phrase FROM local_album_artists "
            "WHERE local_album_id = 'album-1' ORDER BY position"
        ).fetchall()
        track_credits = connection.execute(
            "SELECT credited_name, join_phrase FROM local_track_artists "
            "WHERE local_track_id = 'track-1' ORDER BY position"
        ).fetchall()
        retired = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = 'artist-1'"
        ).fetchone()[0]
        state = connection.execute(
            "SELECT state FROM library_artist_reconciliation_state "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()[0]
    assert album["album_artist_name"] == "Main feat. Guest"
    assert [tuple(row) for row in album_credits] == [
        ("Main", " feat. "),
        ("Guest", ""),
    ]
    assert [tuple(row) for row in track_credits] == [
        ("Main", " feat. "),
        ("Guest", ""),
    ]
    assert retired is None
    assert state == "ambiguous_credit_structure"


@pytest.mark.asyncio
async def test_mixed_mbid_rows_never_retire_the_shared_source(
    store: NativeLibraryStore, db_path: Path
) -> None:
    membership = _membership("1", "Composite")
    shared = membership.album_credits[0]
    membership = CatalogMembership(
        album=membership.album,
        artists=membership.artists,
        tracks=membership.tracks,
        album_credits=[
            LocalArtistCredit(local_artist_id=shared.local_artist_id, position=0),
            LocalArtistCredit(local_artist_id=shared.local_artist_id, position=1),
        ],
        track_credits={
            "track-1": [
                LocalArtistCredit(local_artist_id=shared.local_artist_id, position=0),
                LocalArtistCredit(local_artist_id=shared.local_artist_id, position=1),
            ]
        },
    )
    await store.create_catalog_membership(membership)
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release_with_credits(
        [
            (ARTIST_MBID, "First", "First", " & "),
            (OTHER_ARTIST_MBID, "Second", "Second", ""),
        ]
    )
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")

    with sqlite3.connect(db_path) as connection:
        retired = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = 'artist-1'"
        ).fetchone()[0]
        proof_mbids = {
            row[0]
            for row in connection.execute(
                "SELECT artist_mbid FROM library_artist_credit_proofs "
                "WHERE source_local_artist_id = 'artist-1'"
            )
        }
    assert retired is None
    assert proof_mbids == {ARTIST_MBID, OTHER_ARTIST_MBID}


@pytest.mark.asyncio
async def test_various_artists_identity_gets_the_canonical_catalog_kind(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Various Artists"))
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release(
        artist_mbid=VARIOUS_ARTISTS_MBID, artist_name="Various Artists"
    )
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")

    with sqlite3.connect(db_path) as connection:
        owner = connection.execute(
            "SELECT artist.id, artist.kind FROM local_artists artist "
            "JOIN local_artist_external_identities identity "
            "ON identity.local_artist_id = artist.id "
            "WHERE identity.provider_artist_id = ?",
            (VARIOUS_ARTISTS_MBID,),
        ).fetchone()
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = 'artist-1'"
        ).fetchone()[0]
    assert owner[1] == "various_artists"
    assert retired_into == owner[0]


@pytest.mark.asyncio
async def test_reserved_various_artist_placeholder_cannot_acquire_or_retire_into_real_artist(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Tagged Placeholder"))
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_album_artists SET local_artist_id = ? "
            "WHERE local_album_id = 'album-1'",
            (VARIOUS_ARTISTS_ID,),
        )
        connection.execute(
            "UPDATE local_track_artists SET local_artist_id = ? "
            "WHERE local_track_id = 'track-1'",
            (VARIOUS_ARTISTS_ID,),
        )
        connection.execute(
            "UPDATE local_albums SET album_artist_id = ?, "
            "album_artist_name = 'Various Artists' WHERE id = 'album-1'",
            (VARIOUS_ARTISTS_ID,),
        )
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release(
        artist_mbid=ARTIST_MBID, artist_name="Circa Survive"
    )
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")

    with sqlite3.connect(db_path) as connection:
        reserved = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (VARIOUS_ARTISTS_ID,),
        ).fetchone()[0]
        wrong_identity = connection.execute(
            "SELECT COUNT(*) FROM local_artist_external_identities "
            "WHERE local_artist_id = ? AND provider_artist_id = ?",
            (VARIOUS_ARTISTS_ID, ARTIST_MBID),
        ).fetchone()[0]
        retired_in_action = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE reason_code = "
            "'AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE' "
            "AND after_json LIKE ?",
            (f"%{VARIOUS_ARTISTS_ID}%",),
        ).fetchone()[0]
    assert reserved is None
    assert wrong_identity == 0
    assert retired_in_action == 0


@pytest.mark.asyncio
async def test_automatic_merge_preserves_references_aliases_and_audio_bytes(
    store: NativeLibraryStore, db_path: Path, tmp_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Local Alias"))
    await store.create_catalog_membership(_membership("2", "Canonical Artist"))
    await _accept_exact_identity(store, "1")
    audio_path = tmp_path / "untouched.flac"
    audio_path.write_bytes(b"provider-proof-must-not-touch-audio")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET file_path = ? WHERE id = 'track-1'",
            (str(audio_path),),
        )
        connection.execute(
            "INSERT INTO library_user_favorites VALUES "
            "('admin', 'artist', 'artist-1', 1)"
        )
        connection.execute(
            "INSERT INTO library_play_history "
            "(id, user_id, local_track_id, local_album_id, local_artist_id, "
            "track_name, artist_name, played_at) VALUES "
            "('history-1', 'admin', 'track-1', 'album-1', 'artist-1', "
            "'Track', 'Local Alias', '2026-07-31T00:00:00Z')"
        )
        connection.execute(
            "INSERT INTO library_playlists "
            "(id, name, created_at, updated_at, user_id) VALUES "
            "('playlist-1', 'Proof', 'now', 'now', 'admin')"
        )
        connection.execute(
            "INSERT INTO library_playlist_tracks "
            "(id, playlist_id, position, track_name, artist_name, album_name, "
            "source_type, created_at, local_track_id, local_album_id, local_artist_id) "
            "VALUES ('playlist-track-1', 'playlist-1', 0, 'Track', 'Local Alias', "
            "'Release', 'local', 'now', 'track-1', 'album-1', 'artist-1')"
        )
        connection.execute(
            "INSERT INTO library_compat_id_map VALUES "
            "('11111111111111111111111111111111', 'artist', 'artist-1')"
        )
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")

    with sqlite3.connect(db_path) as connection:
        alias = connection.execute(
            "SELECT local_artist_id FROM local_artist_aliases WHERE alias = 'artist-1'"
        ).fetchone()[0]
        stable = (
            connection.execute("SELECT item_id FROM library_user_favorites").fetchone()[
                0
            ],
            connection.execute(
                "SELECT local_artist_id FROM library_play_history"
            ).fetchone()[0],
            connection.execute(
                "SELECT local_artist_id FROM library_playlist_tracks"
            ).fetchone()[0],
            connection.execute(
                "SELECT internal_id FROM library_compat_id_map"
            ).fetchone()[0],
        )
        stored_path = connection.execute(
            "SELECT file_path FROM local_tracks WHERE id = 'track-1'"
        ).fetchone()[0]
    assert alias == "artist-2"
    assert stable == ("artist-2",) * 4
    assert stored_path == str(audio_path)
    assert audio_path.read_bytes() == b"provider-proof-must-not-touch-audio"


@pytest.mark.asyncio
async def test_stale_catalog_revision_rolls_back_provider_projection(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Artist"))
    await _accept_exact_identity(store, "1")

    async def change_catalog_before_return(*_args, **_kwargs):
        with sqlite3.connect(db_path) as connection:
            connection.execute(
                "UPDATE local_albums SET row_revision = row_revision + 1 "
                "WHERE id = 'album-1'"
            )
        return _release()

    provider = AsyncMock()
    provider.get_canonical_release.side_effect = change_catalog_before_return
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")

    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_artist_credit_proofs"
            ).fetchone()[0]
            == 0
        )
        state = connection.execute(
            "SELECT state, reason_code FROM library_artist_reconciliation_state"
        ).fetchone()
    assert state == ("waiting_for_identity", "STALE_INPUT")


@pytest.mark.asyncio
async def test_local_metadata_policy_never_fetches_missing_provider_proof(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Artist"))
    await _accept_exact_identity(store, "1")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET applied_policy = 'local_metadata' "
            "WHERE id = 'track-1'"
        )
    provider = AsyncMock()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")

    provider.get_canonical_release.assert_not_awaited()
    with sqlite3.connect(db_path) as connection:
        state = connection.execute(
            "SELECT state, reason_code FROM library_artist_reconciliation_state"
        ).fetchone()
    assert state == (
        "waiting_for_identity",
        "LOCAL_METADATA_PROVIDER_LOOKUP_DISABLED",
    )


@pytest.mark.asyncio
async def test_automatic_policy_change_requeues_previously_local_only_evidence(
    store: NativeLibraryStore, db_path: Path
) -> None:
    membership = _membership("1", "Local Artist")
    membership.tracks[0].applied_policy = "local_metadata"
    await store.create_catalog_membership(membership)
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    first = await _run_album(service, store, "album-1")
    provider.get_canonical_release.assert_not_awaited()

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET applied_policy = 'automatic' WHERE id = 'track-1'"
        )

    requeued = await service.enqueue_album("album-1")
    assert requeued is not None
    assert requeued["id"] != first["id"]
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    await service.run_claimed(claimed, "worker")
    provider.get_canonical_release.assert_awaited_once()


@pytest.mark.asyncio
async def test_reprojection_removes_obsolete_artist_credit_proofs(
    store: NativeLibraryStore, db_path: Path
) -> None:
    membership = _membership("1", "Composite")
    shared = membership.album_credits[0]
    membership = CatalogMembership(
        album=membership.album,
        artists=membership.artists,
        tracks=membership.tracks,
        album_credits=[
            LocalArtistCredit(local_artist_id=shared.local_artist_id, position=0),
            LocalArtistCredit(local_artist_id=shared.local_artist_id, position=1),
        ],
        track_credits={
            "track-1": [
                LocalArtistCredit(local_artist_id=shared.local_artist_id, position=0),
                LocalArtistCredit(local_artist_id=shared.local_artist_id, position=1),
            ]
        },
    )
    await store.create_catalog_membership(membership)
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.side_effect = [
        _release_with_credits(
            [
                (ARTIST_MBID, "First", "First", " & "),
                (OTHER_ARTIST_MBID, "Second", "Second", ""),
            ]
        ),
        _release(),
    ]
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    await _run_album(service, store, "album-1")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_album_external_identities SET row_revision = row_revision + 1 "
            "WHERE local_album_id = 'album-1'"
        )
    await _run_album(service, store, "album-1")

    with sqlite3.connect(db_path) as connection:
        proof_positions = connection.execute(
            "SELECT subject_kind, credit_position FROM library_artist_credit_proofs "
            "WHERE local_album_id = 'album-1' ORDER BY subject_kind, credit_position"
        ).fetchall()
    assert proof_positions == [("album", 0), ("track", 0)]


@pytest.mark.asyncio
async def test_repeated_album_and_backfill_triggers_are_idempotent(
    store: NativeLibraryStore,
) -> None:
    await store.create_catalog_membership(_membership("1", "Artist"))
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    first = await service.enqueue_album("album-1")
    repeated = await service.enqueue_album("album-1")
    assert first is not None and repeated is not None
    assert first["id"] == repeated["id"]
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    await service.run_claimed(claimed, "worker")
    after_completion = await service.enqueue_album("album-1")
    assert after_completion is not None
    assert after_completion["id"] == first["id"]
    provider.get_canonical_release.assert_awaited_once()

    payload = "{}"
    await store.put_management_metadata_snapshot(
        LibraryManagementMetadataSnapshot(
            id="artist-credit-snapshot",
            provider="musicbrainz",
            entity_kind="release",
            entity_id=RELEASE_MBID,
            input_hash="a" * 64,
            canonical_payload_json=payload,
            payload_sha256=hashlib.sha256(payload.encode()).hexdigest(),
            fetched_at=4,
        )
    )
    changed_projection = await service.enqueue_album("album-1")
    assert changed_projection is not None
    assert changed_projection["id"] != first["id"]

    backfill = await service.enqueue_backfill()
    repeated_backfill = await service.enqueue_backfill()
    assert backfill["id"] == repeated_backfill["id"]


@pytest.mark.asyncio
async def test_backfill_includes_active_album_without_exact_release(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Artist"))
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    job = await service.enqueue_backfill()
    assert job["expected_work_count"] == 1
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    finished = await service.run_claimed(claimed, "worker")

    with sqlite3.connect(db_path) as connection:
        state = connection.execute(
            "SELECT state, reason_code FROM library_artist_reconciliation_state "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()
    assert finished["state"] == "succeeded"
    assert finished["skipped_count"] == 1
    assert state == ("waiting_for_identity", "EXACT_RELEASE_NOT_ACCEPTED")


@pytest.mark.asyncio
async def test_group_pagination_and_dismissal_revision_guard(
    store: NativeLibraryStore, db_path: Path
) -> None:
    for suffix, name in enumerate(
        ("Alpha", "Alpha", "Beta", "Beta", "Gamma", "Gamma"), 1
    ):
        await store.create_catalog_membership(_membership(str(suffix), name))
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    first = await service.list_groups(limit=1, cursor=None, state=None, search=None)
    assert first.total == 3
    assert first.has_more is True
    assert first.next_cursor == first.items[0].id
    second = await service.list_groups(
        limit=1, cursor=first.next_cursor, state=None, search=None
    )
    assert second.items[0].display_name != first.items[0].display_name

    detail = await service.group_detail(first.items[0].id)
    changed_id = detail.members[0].id
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_artists SET row_revision = row_revision + 1 WHERE id = ?",
            (changed_id,),
        )
    with pytest.raises(StaleRevisionError):
        await service.dismiss_group(
            detail.id,
            detail.member_revisions,
            "admin",
        )


@pytest.mark.asyncio
async def test_duplicate_groups_exclude_artist_owned_only_by_retired_album(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Duplicate"))
    await store.create_catalog_membership(_membership("2", "Duplicate"))
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET retired_into_album_id = 'album-2' "
            "WHERE id = 'album-1'"
        )
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    response = await service.list_groups(
        limit=10, cursor=None, state=None, search="Duplicate"
    )

    assert response.total == 0


@pytest.mark.asyncio
async def test_duplicate_groups_and_release_links_exclude_zero_track_album_remnants(
    store: NativeLibraryStore,
) -> None:
    await store.create_catalog_membership(_membership("1", "Duplicate"))
    await store.create_catalog_membership(_membership("2", "Duplicate"))
    ghost = _membership("3", "Duplicate")
    await store.create_catalog_membership(
        CatalogMembership(
            album=ghost.album,
            artists=ghost.artists,
            tracks=[],
            album_credits=ghost.album_credits,
            track_credits={},
        )
    )
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    response = await service.list_groups(
        limit=10, cursor=None, state=None, search="Duplicate"
    )

    assert response.total == 1
    group = response.items[0]
    assert {member.id for member in group.members} == {"artist-1", "artist-2"}
    detail = await service.group_detail(group.id)
    assert {release.id for release in detail.releases} == {"album-1", "album-2"}


@pytest.mark.asyncio
async def test_shared_operation_supervisor_dispatches_reconciliation_repair() -> None:
    store = AsyncMock()
    job = {"id": "reconciliation-job", "kind": "repair"}
    store.claim_operation_job.side_effect = [None, None, job]
    store.get_operation_snapshot.return_value = {
        "snapshot": {
            "scope_json": '{"purpose":"artist_identity_reconciliation"}',
            "phase": "audit",
        }
    }
    operations = Mock()
    operations.response_for.return_value = "response"
    reconciliation = AsyncMock()
    reconciliation.run_claimed.return_value = {"id": job["id"], "state": "succeeded"}
    supervisor = LibraryOperationSupervisor(
        store,
        operations,
        AsyncMock(),
        AsyncMock(),
        artist_reconciliation=reconciliation,
    )

    result = await supervisor.run_once("worker", now=3)

    assert result == "response"
    reconciliation.run_claimed.assert_awaited_once_with(job, "worker")


@pytest.mark.asyncio
async def test_stored_revision_valid_proof_row_authorizes_the_anchor(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """The durable album-level proof row (recorded by credit projection) also
    satisfies the gate when no accepted identity remains on the album."""
    import hashlib

    survivor_id, source_id, album_id = await _seed_provider_anchored_split(store)
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id=album_id,
            release_group_mbid=RELEASE_GROUP_MBID,
            release_mbid=RELEASE_MBID,
            selected_at=2,
        ),
        expected_album_revision=1,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_artist_credit_proofs "
            "(subject_kind, subject_id, local_album_id, local_track_id, credit_position, "
            "source_local_artist_id, local_artist_id, artist_mbid, canonical_name, "
            "credited_name, sort_name, join_phrase, release_mbid, release_track_mbid, "
            "album_identity_revision, track_identity_revision, evidence_hash, created_at, "
            "updated_at) VALUES ('track', 'track-split', 'album-split', 'track-split', 0, "
            "?, ?, ?, 'Same Artist', 'Same Artist', 'Artist, Same', '', ?, ?, 1, 1, ?, 5, 5)",
            (
                source_id,
                survivor_id,
                ARTIST_MBID,
                RELEASE_MBID,
                RELEASE_TRACK_MBID,
                hashlib.sha256(b"evidence").hexdigest(),
            ),
        )
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    await _run_album(service, store, album_id)

    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (source_id,),
        ).fetchone()[0]
    assert retired_into == survivor_id


@pytest.mark.asyncio
async def test_conflicting_current_proof_row_blocks_the_anchor_retirement(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A current, revision-valid contradictory proof row vetoes the retirement
    even though the album carries an accepted release identity."""
    import hashlib

    survivor_id, source_id, album_id = await _seed_provider_anchored_split(store)
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id=album_id,
            release_group_mbid=RELEASE_GROUP_MBID,
            release_mbid=RELEASE_MBID,
            selected_at=2,
        ),
        expected_album_revision=1,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_artist_credit_proofs "
            "(subject_kind, subject_id, local_album_id, local_track_id, credit_position, "
            "source_local_artist_id, local_artist_id, artist_mbid, canonical_name, "
            "credited_name, sort_name, join_phrase, release_mbid, release_track_mbid, "
            "album_identity_revision, track_identity_revision, evidence_hash, created_at, "
            "updated_at) VALUES ('track', 'track-split', 'album-split', 'track-split', 0, "
            "?, ?, ?, 'Other Artist', 'Other Artist', 'Artist, Other', '', ?, ?, 1, 1, ?, 5, 5)",
            (
                source_id,
                survivor_id,
                OTHER_ARTIST_MBID,
                RELEASE_MBID,
                RELEASE_TRACK_MBID,
                hashlib.sha256(b"evidence").hexdigest(),
            ),
        )
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    await _run_album(service, store, album_id)

    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (source_id,),
        ).fetchone()[0]
        action_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE reason_code = 'AUTOMATIC_PROVIDER_ANCHORED_ARTIST_CONVERGENCE'"
        ).fetchone()[0]
    assert retired_into is None
    assert action_count == 0


@pytest.mark.asyncio
async def test_stale_proof_row_without_accepted_identity_does_not_retire(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A stored proof row only counts while it is valid against the current
    identity revisions; with the accepted identity gone, a row recorded
    against the old revision cannot satisfy the gate."""
    import hashlib

    survivor_id, source_id, album_id = await _seed_provider_anchored_split(store)
    # A proof row left behind by a since-cleared accepted identity: its
    # album_identity_revision no longer joins any current identity row.
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_artist_credit_proofs "
            "(subject_kind, subject_id, local_album_id, local_track_id, credit_position, "
            "source_local_artist_id, local_artist_id, artist_mbid, canonical_name, "
            "credited_name, sort_name, join_phrase, release_mbid, release_track_mbid, "
            "album_identity_revision, track_identity_revision, evidence_hash, created_at, "
            "updated_at) VALUES ('track', 'track-split', 'album-split', 'track-split', 0, "
            "?, ?, ?, 'Same Artist', 'Same Artist', 'Artist, Same', '', ?, ?, 1, 1, ?, 5, 5)",
            (
                source_id,
                survivor_id,
                ARTIST_MBID,
                RELEASE_MBID,
                RELEASE_TRACK_MBID,
                hashlib.sha256(b"evidence").hexdigest(),
            ),
        )
    service = ArtistIdentityReconciliationService(store, AsyncMock(), clock=lambda: 3)

    await _run_album(service, store, album_id)

    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (source_id,),
        ).fetchone()[0]
    assert retired_into is None


@pytest.mark.asyncio
async def test_album_reenqueue_with_moved_revision_joins_queued_job(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Artist"))
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    first = await service.enqueue_album("album-1")
    assert first is not None
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_album_external_identities SET row_revision = row_revision + 1 "
            "WHERE local_album_id = 'album-1'"
        )
    second = await service.enqueue_album("album-1")
    assert second is not None
    assert second["id"] == first["id"]
    with sqlite3.connect(db_path) as connection:
        count = connection.execute(
            "SELECT COUNT(*) FROM library_operation_jobs WHERE kind = 'repair'"
        ).fetchone()[0]
    assert count == 1


@pytest.mark.asyncio
async def test_album_reenqueue_after_terminal_state_creates_fresh_job(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("1", "Artist"))
    await _accept_exact_identity(store, "1")
    provider = AsyncMock()
    provider.get_canonical_release.return_value = _release()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)

    first = await service.enqueue_album("album-1")
    assert first is not None
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_operation_jobs SET state = 'succeeded', "
            "terminal_code = 'RECONCILIATION_COMPLETED', terminal_at = 4 WHERE id = ?",
            (first["id"],),
        )
        connection.execute(
            "UPDATE local_album_external_identities SET row_revision = row_revision + 1 "
            "WHERE local_album_id = 'album-1'"
        )
    second = await service.enqueue_album("album-1")
    assert second is not None
    assert second["id"] != first["id"]
    with sqlite3.connect(db_path) as connection:
        count = connection.execute(
            "SELECT COUNT(*) FROM library_operation_jobs WHERE kind = 'repair'"
        ).fetchone()[0]
    assert count == 2
