import hashlib
import sqlite3
import threading
from pathlib import Path

import msgspec
import pytest

from core.exceptions import StaleRevisionError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.audio import AudioInfo, AudioTag
from models.edition_management import (
    EditionConversionJob,
    EditionConversionLocalFile,
    EditionConversionTarget,
)
from models.library_management import (
    LibraryManagementImportBundle,
    LibraryManagementImportBundleRecord,
    LibraryManagementImportFile,
    LibraryManagementImportJournal,
)
from models.library_work import ScannedTrackWrite
from models.local_catalog import (
    LocalAlbum,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
)
from services.native.identification_revisions import (
    album_identity_revision,
    album_input_revisions,
)


def _seed(path: Path) -> None:
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users(id) VALUES ('admin')")
    NativeLibraryStore(path, threading.Lock())
    with sqlite3.connect(path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO local_artists "
            "(id,display_name,folded_name,normalized_name,kind,created_at,updated_at) "
            "VALUES ('artist','Artist','artist','artist','person',1,1)"
        )
        connection.execute(
            "INSERT INTO local_albums "
            "(id,root_id,grouping_key,title,title_folded,album_artist_name,"
            "album_artist_name_folded,album_artist_id,grouping_source,created_at,updated_at) "
            "VALUES ('album','root','group','Album','album','Artist','artist','artist',"
            "'automatic',1,1)"
        )
        connection.execute(
            "INSERT INTO local_tracks "
            "(id,local_album_id,root_id,file_path,relative_path,path_hash,file_size_bytes,"
            "file_mtime_ns,stat_revision,stat_revision_kind,tag_revision,title,title_folded,"
            "artist_name,artist_name_folded,album_title,album_title_folded,album_artist_name,"
            "album_artist_name_folded,disc_number,track_number,file_format,ingest_source,"
            "imported_at,membership_source) VALUES ('track','album','root','/music/a.flac',"
            "'a.flac','hash',1,1,'stat','exact','tag','Track','track','Artist','artist',"
            "'Album','album','Artist','artist',1,1,'flac','scan',1,'automatic')"
        )


@pytest.mark.asyncio
async def test_edition_conversion_store_is_idempotent_and_round_trips(
    tmp_path: Path,
) -> None:
    path = tmp_path / "library.db"
    _seed(path)
    first = NativeLibraryStore(path, threading.Lock())
    second = NativeLibraryStore(path, threading.Lock())
    job = EditionConversionJob(
        id="job",
        local_album_id="album",
        target_release_group_mbid="group-mbid",
        target_release_mbid="release-mbid",
        target_album_title="Album",
        target_artist_name="Artist",
        state="preflight",
        expected_album_revision=1,
        expected_input_revision="input",
        expected_identity_revision="identity",
        preflight_token_hash="hash",
        download_source_ready=True,
        required_temporary_bytes=1,
        kept_count=1,
        acquire_count=0,
        recycle_count=0,
        staged_count=0,
        failed_count=0,
        final_preview_job_id=None,
        final_preview_token_hash=None,
        final_bundle_json=None,
        final_bundle_hash=None,
        requested_by_user_id="admin",
        error_code=None,
        created_at=1,
        updated_at=1,
    )
    target = EditionConversionTarget(
        job_id="job",
        ordinal=0,
        disc_number=1,
        track_number=1,
        release_track_mbid="release-track",
        recording_mbid="recording",
        title="Track",
        duration_seconds=1,
        state="kept",
        kept_local_track_id="track",
    )
    local = EditionConversionLocalFile(
        job_id="job",
        local_track_id="track",
        action="keep",
        target_ordinal=0,
        evidence_kind="recording",
        expected_track_revision=1,
        expected_identity_revision=None,
        expected_stat_revision="stat",
    )

    created = await first.create_edition_conversion(job, (target,), (local,))
    loaded = await second.get_edition_conversion(created.id)

    assert loaded == created
    assert loaded is not None
    assert loaded.artifacts == ()


def test_custom_manifest_rows_are_immutable_across_store_reconstruction(
    tmp_path: Path,
) -> None:
    path = tmp_path / "library.db"
    _seed(path)
    with sqlite3.connect(path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO library_custom_edition_manifests "
            "(id,local_album_id,version,release_group_mbid,album_title,album_artist_name,"
            "album_metadata_json,source_album_revision,input_revision,content_hash,"
            "sealed_by_user_id,sealed_at) VALUES "
            "('manifest','album',1,'group','Album','Artist','{}',1,'input','content',"
            "'admin',1)"
        )
        connection.execute(
            "INSERT INTO library_custom_edition_tracks "
            "(manifest_id,ordinal,local_track_id,source_track_revision,stat_revision,"
            "tag_revision,title,artist_name,album_title,album_artist_name,disc_number,"
            "track_number) VALUES "
            "('manifest',0,'track',1,'stat','tag','Track','Artist','Album','Artist',1,1)"
        )
    NativeLibraryStore(path, threading.Lock())

    with sqlite3.connect(path) as connection:
        with pytest.raises(sqlite3.IntegrityError, match="immutable"):
            connection.execute(
                "UPDATE library_custom_edition_manifests SET album_title='Changed' "
                "WHERE id='manifest'"
            )
        with pytest.raises(sqlite3.IntegrityError, match="immutable"):
            connection.execute(
                "DELETE FROM library_custom_edition_tracks WHERE manifest_id='manifest'"
            )


@pytest.mark.asyncio
async def test_failed_download_association_can_be_retried_without_poisoning_target(
    tmp_path: Path,
) -> None:
    path = tmp_path / "library.db"
    _seed(path)
    store = NativeLibraryStore(path, threading.Lock())
    job = EditionConversionJob(
        id="retry-job",
        local_album_id="album",
        target_release_group_mbid="group-mbid",
        target_release_mbid="release-mbid",
        target_album_title="Album",
        target_artist_name="Artist",
        state="preflight",
        expected_album_revision=1,
        expected_input_revision="input",
        expected_identity_revision="identity",
        preflight_token_hash="hash",
        download_source_ready=True,
        required_temporary_bytes=1,
        kept_count=0,
        acquire_count=1,
        recycle_count=1,
        staged_count=0,
        failed_count=0,
        final_preview_job_id=None,
        final_preview_token_hash=None,
        final_bundle_json=None,
        final_bundle_hash=None,
        requested_by_user_id="admin",
        error_code=None,
        created_at=1,
        updated_at=1,
    )
    target = EditionConversionTarget(
        job_id=job.id,
        ordinal=0,
        disc_number=1,
        track_number=1,
        release_track_mbid="release-track",
        recording_mbid="recording",
        title="Track",
        duration_seconds=1,
        state="pending",
    )
    local = EditionConversionLocalFile(
        job_id=job.id,
        local_track_id="track",
        action="recycle_extra",
        target_ordinal=None,
        evidence_kind="none",
        expected_track_revision=1,
        expected_identity_revision=None,
        expected_stat_revision="stat",
    )
    await store.create_edition_conversion(job, (target,), (local,))
    started = await store.start_edition_conversion(
        job.id,
        expected_row_revision=1,
        preflight_token_hash="hash",
        now=2,
    )
    await store.associate_edition_conversion_download(
        job.id, 0, source_kind="download", task_id="first-task", now=3
    )
    await store.fail_edition_conversion_target(
        job.id, 0, code="ACQUISITION_FAILED", now=4
    )
    failed = await store.get_edition_conversion(job.id)
    assert failed is not None
    retried = await store.reset_edition_conversion_targets(
        job.id,
        (0,),
        expected_row_revision=failed.row_revision,
        now=5,
    )

    await store.associate_edition_conversion_download(
        job.id, 0, source_kind="download", task_id="second-task", now=6
    )
    associations = await store.list_edition_conversion_downloads(job.id)

    assert started.state == "acquiring"
    assert retried.targets[0].state == "pending"
    assert [(row["task_id"], row["state"]) for row in associations] == [
        ("first-task", "superseded"),
        ("second-task", "active"),
    ]


@pytest.mark.asyncio
async def test_cancelled_conversion_rejects_late_download_association(
    tmp_path: Path,
) -> None:
    path = tmp_path / "library.db"
    _seed(path)
    store = NativeLibraryStore(path, threading.Lock())
    job = EditionConversionJob(
        id="cancel-job",
        local_album_id="album",
        target_release_group_mbid="group-mbid",
        target_release_mbid="release-mbid",
        target_album_title="Album",
        target_artist_name="Artist",
        state="preflight",
        expected_album_revision=1,
        expected_input_revision="input",
        expected_identity_revision="identity",
        preflight_token_hash="hash",
        download_source_ready=True,
        required_temporary_bytes=1,
        kept_count=0,
        acquire_count=1,
        recycle_count=1,
        staged_count=0,
        failed_count=0,
        final_preview_job_id=None,
        final_preview_token_hash=None,
        final_bundle_json=None,
        final_bundle_hash=None,
        requested_by_user_id="admin",
        error_code=None,
        created_at=1,
        updated_at=1,
    )
    target = EditionConversionTarget(
        job_id=job.id,
        ordinal=0,
        disc_number=1,
        track_number=1,
        release_track_mbid="release-track",
        recording_mbid="recording",
        title="Track",
        duration_seconds=1,
        state="pending",
    )
    local = EditionConversionLocalFile(
        job_id=job.id,
        local_track_id="track",
        action="recycle_extra",
        target_ordinal=None,
        evidence_kind="none",
        expected_track_revision=1,
        expected_identity_revision=None,
        expected_stat_revision="stat",
    )
    await store.create_edition_conversion(job, (target,), (local,))
    started = await store.start_edition_conversion(
        job.id,
        expected_row_revision=1,
        preflight_token_hash="hash",
        now=2,
    )
    await store.cancel_edition_conversion(
        job.id, expected_row_revision=started.row_revision, now=3
    )

    with pytest.raises(StaleRevisionError):
        await store.associate_edition_conversion_download(
            job.id, 0, source_kind="download", task_id="late-task", now=4
        )


def test_store_clears_legacy_plaintext_conversion_preview_tokens(
    tmp_path: Path,
) -> None:
    path = tmp_path / "library.db"
    _seed(path)
    with sqlite3.connect(path) as connection:
        connection.execute(
            "ALTER TABLE library_edition_conversion_jobs "
            "ADD COLUMN final_preview_token TEXT"
        )
        connection.execute(
            "INSERT INTO library_edition_conversion_jobs "
            "(id,local_album_id,target_release_group_mbid,target_release_mbid,"
            "target_album_title,target_artist_name,state,expected_album_revision,"
            "expected_input_revision,expected_identity_revision,preflight_token_hash,"
            "download_source_ready,required_temporary_bytes,kept_count,acquire_count,"
            "recycle_count,staged_count,failed_count,requested_by_user_id,created_at,"
            "updated_at,final_preview_token) VALUES "
            "('legacy','album','group','release','Album','Artist','cancelled',1,"
            "'input','identity','hash',1,1,0,1,1,0,0,'admin',1,1,'plaintext-secret')"
        )

    NativeLibraryStore(path, threading.Lock())

    with sqlite3.connect(path) as connection:
        token = connection.execute(
            "SELECT final_preview_token FROM library_edition_conversion_jobs "
            "WHERE id='legacy'"
        ).fetchone()[0]
    assert token is None


def _conversion_write(track_id: str) -> ScannedTrackWrite:
    artist = LocalArtist(
        id="artist",
        display_name="Artist",
        folded_name="artist",
        normalized_name="artist",
        kind="person",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id="album",
        root_id="root",
        grouping_key="group",
        title="Album",
        album_artist_id="artist",
        album_artist_name="Artist",
        created_at=1,
        updated_at=1,
    )
    track = LocalTrack(
        id=track_id,
        local_album_id="album",
        root_id="root",
        file_path="/music/a.flac",
        relative_path="a.flac",
        path_hash="hash",
        file_size_bytes=1,
        file_mtime_ns=1,
        stat_revision="stat",
        title="Track",
        artist_name="Artist",
        album_title="Album",
        album_artist_name="Artist",
        file_format="flac",
        imported_at=1,
    )
    return ScannedTrackWrite(
        artist=artist,
        album=album,
        track=track,
        credit=LocalArtistCredit(local_artist_id="artist", position=0),
        root_id="root",
        relative_path="a.flac",
        comparison_result="new",
        grouping_context="test",
    )


@pytest.mark.asyncio
async def test_conversion_commit_accepts_source_track_without_identity(
    tmp_path: Path,
) -> None:
    """A conversion Apply commits for an un-identified indexed source track."""
    path = tmp_path / "library.db"
    _seed(path)
    store = NativeLibraryStore(path, threading.Lock())
    with sqlite3.connect(path) as connection:
        # final_preview_job_id references library_operation_jobs (RESTRICT).
        connection.execute(
            "INSERT INTO library_operation_jobs (id,kind,state,created_at,updated_at) "
            "VALUES ('preview','library_management','ready',1,1)"
        )

    context = await store.get_album_identification_context("album")
    assert context is not None
    tracks = context["tracks"]
    assert [track["identity_row_revision"] for track in tracks] == [None]

    request = LibraryManagementImportFile(
        ordinal=0,
        input_path="/incoming/Track.flac",
        destination_root_id="root",
        destination_relative_path="Converted/Track.flac",
        tag=AudioTag(title="Track", artist="Artist", album="Album", track_number=1),
        info=AudioInfo(
            duration_seconds=180.0,
            bitrate=800,
            sample_rate=44100,
            channels=2,
            file_format="flac",
            file_size_bytes=100,
        ),
        release_group_mbid=None,
        release_mbid=None,
        recording_mbid=None,
        confidence=0.0,
        source="edition_conversion",
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="edition-conversion:null-identity",
        origin="edition_conversion",
        policy_revision="policy-1",
        files=(request,),
        conversion_job_id="job",
        conversion_expected_row_revision=1,
        conversion_local_album_id="album",
        conversion_preview_job_id="preview",
    )
    request_json = msgspec.json.encode(bundle).decode()
    request_hash = hashlib.sha256(request_json.encode()).hexdigest()
    await store.ensure_library_management_import_bundle(
        LibraryManagementImportBundleRecord(
            id="bundle",
            idempotency_key=bundle.idempotency_key,
            origin="acquisition",
            policy_revision="policy-1",
            request_json=request_json,
            request_hash=request_hash,
            state="publishing",
            created_at=1,
            updated_at=1,
        )
    )
    await store.ensure_library_management_import_journal(
        LibraryManagementImportJournal(
            bundle_id="bundle",
            ordinal=0,
            state="published",
            source_fingerprint="a" * 64,
            source_size=1,
            source_mtime_ns=1,
            temporary_relative_path="conversion/track.flac",
            destination_root_id="root",
            destination_relative_path="Converted/Track.flac",
        )
    )
    local_track = tracks[0]
    await store.create_edition_conversion(
        EditionConversionJob(
            id="job",
            local_album_id="album",
            target_release_group_mbid="group",
            target_release_mbid="release",
            target_album_title="Album",
            target_artist_name="Artist",
            state="ready",
            expected_album_revision=int(context["album"]["row_revision"]),
            expected_input_revision=":".join(album_input_revisions(tracks)),
            expected_identity_revision=album_identity_revision(
                context["identity"], tracks
            ),
            preflight_token_hash="hash",
            download_source_ready=True,
            required_temporary_bytes=1,
            kept_count=1,
            acquire_count=0,
            recycle_count=0,
            staged_count=0,
            failed_count=0,
            final_preview_job_id="preview",
            final_preview_token_hash="preview-hash",
            final_bundle_json=request_json,
            final_bundle_hash=request_hash,
            requested_by_user_id="admin",
            error_code=None,
            created_at=1,
            updated_at=1,
        ),
        (
            EditionConversionTarget(
                job_id="job",
                ordinal=0,
                disc_number=1,
                track_number=1,
                release_track_mbid="release-track",
                recording_mbid="recording",
                title="Track",
                duration_seconds=1,
                state="kept",
                kept_local_track_id="track",
            ),
        ),
        (
            EditionConversionLocalFile(
                job_id="job",
                local_track_id="track",
                action="keep",
                target_ordinal=0,
                evidence_kind="recording",
                expected_track_revision=int(local_track["row_revision"]),
                expected_identity_revision=None,
                expected_stat_revision="stat",
            ),
        ),
    )

    track_ids = await store.commit_library_management_import_bundle(
        "bundle",
        writes=[(0, _conversion_write("track"))],
        replacement_track_ids={},
        recycle_track_ids={},
        requests_by_ordinal={0: request},
        automatic_requests={},
        expected_policy_revision="policy-1",
        result_paths_by_ordinal={0: "Converted/Track.flac"},
        updated_at=10.0,
    )

    assert track_ids == ("track",)
    committed = await store.get_library_management_import_bundle("bundle")
    assert committed is not None
    assert committed.state == "catalog_committed"


_PREVIEW_TOKEN_HASH = "t" * 64


def _insert_edition_preview(
    path: Path,
    *,
    preview_id: str = "preview",
    op_state: str = "ready",
    snapshot_phase: str = "ready",
    idempotency_key: str | None = "edition-conversion-preview:job",
) -> None:
    with sqlite3.connect(path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO library_operation_jobs "
            "(id,kind,state,idempotency_key,created_at,updated_at) "
            "VALUES (?,?,?,?,?,?)",
            (preview_id, "library_management", op_state, idempotency_key, 1, 1),
        )
        connection.execute(
            "INSERT INTO library_management_job_snapshots "
            "(job_id,mode,origin,phase,selection_json,profile_revision,"
            "settings_revision,naming_revision,policy_revision,catalog_revision,"
            "profile_snapshot_json,preview_token_hash,preview_created_at,"
            "preview_expires_at,created_at,updated_at) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                preview_id,
                "preview",
                "manual",
                snapshot_phase,
                "{}",
                "p",
                "s",
                "n",
                "p",
                1,
                "{}",
                _PREVIEW_TOKEN_HASH,
                1,
                9999999999.0,
                1,
                1,
            ),
        )


def _ready_edition_job(
    job_id: str = "job",
    preview_id: str = "preview",
    *,
    staged_artifact_id: str | None = None,
) -> tuple[EditionConversionJob, EditionConversionTarget, EditionConversionLocalFile]:
    job = EditionConversionJob(
        id=job_id,
        local_album_id="album",
        target_release_group_mbid="group",
        target_release_mbid="release",
        target_album_title="Album",
        target_artist_name="Artist",
        state="ready",
        expected_album_revision=1,
        expected_input_revision="input",
        expected_identity_revision="identity",
        preflight_token_hash="hash",
        download_source_ready=True,
        required_temporary_bytes=1,
        kept_count=1,
        acquire_count=0,
        recycle_count=0,
        staged_count=0,
        failed_count=0,
        final_preview_job_id=preview_id,
        final_preview_token_hash=_PREVIEW_TOKEN_HASH,
        final_bundle_json="{}",
        final_bundle_hash="h" * 64,
        requested_by_user_id="admin",
        error_code=None,
        created_at=1,
        updated_at=1,
    )
    target = EditionConversionTarget(
        job_id=job_id,
        ordinal=0,
        disc_number=1,
        track_number=1,
        release_track_mbid="release-track",
        recording_mbid="recording",
        title="Track",
        duration_seconds=1,
        state="kept",
        kept_local_track_id="track",
        staged_artifact_id=staged_artifact_id,
    )
    local = EditionConversionLocalFile(
        job_id=job_id,
        local_track_id="track",
        action="keep",
        target_ordinal=0,
        evidence_kind="recording",
        expected_track_revision=1,
        expected_identity_revision=None,
        expected_stat_revision="stat",
    )
    return job, target, local


@pytest.mark.asyncio
async def test_cancel_edition_conversion_recovers_after_preview_discard(
    tmp_path: Path,
) -> None:
    """Issue #425: cancel must tolerate an already-discarded final preview."""
    path = tmp_path / "library.db"
    _seed(path)
    _insert_edition_preview(path)
    store = NativeLibraryStore(path, threading.Lock())
    job, target, local = _ready_edition_job()
    await store.create_edition_conversion(job, (target,), (local,))
    discarded = await store.discard_library_management_preview(
        "preview", expected_job_revision=1, now=2.0
    )
    assert discarded["state"] == "cancelled"

    cancelled = await store.cancel_edition_conversion(
        "job", expected_row_revision=1, now=3.0
    )

    assert cancelled.state == "cancelled"


@pytest.mark.asyncio
async def test_cancel_edition_conversion_refuses_mid_apply_preview(
    tmp_path: Path,
) -> None:
    """Cancel must not race an in-flight Apply of the final preview."""
    path = tmp_path / "library.db"
    _seed(path)
    _insert_edition_preview(path, op_state="running", snapshot_phase="applying")
    store = NativeLibraryStore(path, threading.Lock())
    job, target, local = _ready_edition_job()
    await store.create_edition_conversion(job, (target,), (local,))

    with pytest.raises(StaleRevisionError):
        await store.cancel_edition_conversion("job", expected_row_revision=1, now=2.0)

    current = await store.get_edition_conversion("job")
    assert current is not None
    assert current.state == "ready"


@pytest.mark.asyncio
async def test_detach_dead_preview_frees_link_and_idempotency_key(
    tmp_path: Path,
) -> None:
    """Issue #425: detach clears the dead link so a fresh preview can seal."""
    path = tmp_path / "library.db"
    _seed(path)
    _insert_edition_preview(path)
    store = NativeLibraryStore(path, threading.Lock())
    job, target, local = _ready_edition_job(staged_artifact_id="artifact-1")
    await store.create_edition_conversion(job, (target,), (local,))
    await store.discard_library_management_preview(
        "preview", expected_job_revision=1, now=2.0
    )

    detached = await store.detach_dead_edition_conversion_preview(
        "job", expected_row_revision=1, now=3.0
    )

    assert detached.final_preview_job_id is None
    assert detached.final_preview_token_hash is None
    assert detached.final_bundle_json is None
    assert detached.final_bundle_hash is None
    assert detached.row_revision == 2
    with sqlite3.connect(path) as connection:
        key = connection.execute(
            "SELECT idempotency_key FROM library_operation_jobs WHERE id='preview'"
        ).fetchone()[0]
    assert key is None

    _insert_edition_preview(
        path,
        preview_id="preview-2",
        idempotency_key="edition-conversion-preview:job",
    )
    bundle_json = "{}"
    resealed = await store.seal_edition_conversion_preview(
        "job",
        expected_row_revision=detached.row_revision,
        preview_job_id="preview-2",
        preview_token_hash="n" * 64,
        bundle_json=bundle_json,
        bundle_hash=hashlib.sha256(bundle_json.encode()).hexdigest(),
        now=4.0,
    )
    assert resealed.final_preview_job_id == "preview-2"


@pytest.mark.asyncio
async def test_rotate_preview_capability_still_works_for_live_preview(
    tmp_path: Path,
) -> None:
    """A live final preview keeps the rotate path (no detach)."""
    path = tmp_path / "library.db"
    _seed(path)
    _insert_edition_preview(path)
    store = NativeLibraryStore(path, threading.Lock())
    job, target, local = _ready_edition_job()
    await store.create_edition_conversion(job, (target,), (local,))

    rotated = await store.rotate_edition_conversion_preview_capability(
        "job",
        expected_row_revision=1,
        preview_token_hash="n" * 64,
        preview_expires_at=9999999999.0,
        now=2.0,
    )

    assert rotated.final_preview_job_id == "preview"
    assert rotated.final_preview_token_hash == "n" * 64
