"""Plugin acquisition gates: unknown sources never resolve, attempts carry the
plugin key, and one broken plugin indexer drops only its own group.

Covers plan section 7 item 15 gates 3-4 with kept regression tests:
(a) unknown/disabled/missing sources raise domain no-source/unknown-source
from every resolver (never the soulseek strategy), (b) attempt/manifest/
handle persistence for a plugin task carries ``plugin:<key>``, (c) the
manual-search fan-out (D15) drops only the broken plugin group, (d) plugin
candidates surface grouped under ``plugin:<key>``.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from models.download import ScoredCandidate, TargetAlbum
from models.download_identity import plugin_identity
from models.download_manifest import ManifestCodec
from repositories.protocols.download_client import TaskHandle
from repositories.protocols.indexer import IndexerResult, PluginSearchResult
from services.native.acquisition.errors import OrchestrationError
from services.native.acquisition.plugin_strategy import PluginSourceStrategy
from services.native.download_orchestrator import DownloadOrchestrator
from services.native.download_service import DownloadService
from services.plugin_sources import PluginSourceRegistry

_TEMPLATE = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"
GOOD_KEY = "plugin:goodplug"

# Unknown + disabled + missing keys. "" exercises the no-source branch, the
# rest the unknown-source branch.
BAD_SOURCES = ["plugin:ghost", "plugin:offplug", "bogus", ""]


class _FakeManifest:
    def __init__(self, name, display_name=""):
        self.name = name
        self.display_name = display_name
        self.capability_configs = []


class _FakeInstance:
    """Plugin instance double: configured flag + scripted album search."""

    def __init__(self, configured=True, releases=None, exc=None):
        self._configured = configured
        self._releases = list(releases or [])
        self._exc = exc

    def is_configured(self):
        return self._configured

    async def search_album(
        self, artist_name, album_title, year=None, track_count=None, *, timeout=30.0
    ):
        if self._exc is not None:
            raise self._exc
        return list(self._releases)


class _FakePlugin:
    def __init__(self, name, *, caps=("download_client", "indexer"), enabled=True, instance=None):
        self.manifest = _FakeManifest(name)
        self.active_capabilities = set(caps)
        self.enabled = enabled
        self.instance = instance


class _StubHost:
    """Minimal PluginHost surface: download_clients/indexers/generation."""

    def __init__(self, plugins, generation=7):
        self._plugins = list(plugins)
        self.generation = generation

    def download_clients(self):
        return [p for p in self._plugins if "download_client" in p.active_capabilities]

    def indexers(self):
        return [p for p in self._plugins if "indexer" in p.active_capabilities]


def _registry(*plugins):
    return PluginSourceRegistry(_StubHost(list(plugins)))


def _gate_registry():
    """One good source plus one disabled decoy (never surfaces as a spec)."""
    good = _FakePlugin("goodplug", instance=_FakeInstance(configured=True))
    off = _FakePlugin("offplug", enabled=False, instance=_FakeInstance(configured=True))
    return _registry(good, off)


def _stub_orchestrator(plugin_sources, staging):
    client = MagicMock()
    client.is_configured.return_value = True
    return DownloadOrchestrator(
        client=client,
        indexer=MagicMock(),
        download_store=AsyncMock(),
        file_processor=MagicMock(),
        library_manager=MagicMock(),
        scorer=MagicMock(),
        track_matcher=MagicMock(),
        manifest_codec=ManifestCodec(),
        event_bus=MagicMock(),
        staging_path=staging,
        naming_template=_TEMPLATE,
        plugin_sources=plugin_sources,
    )


def _task_ns(source):
    return SimpleNamespace(source=source)


def _candidate_ns(source):
    return SimpleNamespace(source=source, username="plugpeer", plugin_release=None)


def _plugin_release(title="Good Release"):
    return PluginSearchResult(
        title=title,
        size_bytes=80_000_000,
        score=0.95,
        quality_tier="lossless",
        files=[],
        payload="tok-1",
    )


def _indexer_hit(key, title="Good Release"):
    return IndexerResult(source=key, plugin=_plugin_release(title))


def _service(*, registry, scorer, store=None):
    store = store or AsyncMock()
    store.load_quarantine_set.return_value = set()
    return DownloadService(
        MagicMock(),
        AsyncMock(),
        AsyncMock(),
        AsyncMock(),
        store,
        AsyncMock(),
        MagicMock(),
        soulseek_enabled=False,
        plugin_sources=registry,
        plugin_scorer=scorer,
    ), store


def _rank_good_only(good_key):
    """Scorer stub: the broken key raises, the good key yields one candidate."""

    async def _rank(target, releases, **kwargs):
        key = kwargs.get("source_key", "")
        if key != good_key:
            raise RuntimeError(f"boom {key}")
        return [
            ScoredCandidate(
                source=key,
                username="plugpeer",
                parent_directory="G - R",
                files=[],
                coherence=0.9,
                file_confidence=0.9,
                final_score=0.9,
                tier="auto",
            )
        ]

    scorer = AsyncMock()
    scorer.rank.side_effect = _rank
    return scorer


# (a) unknown/disabled/missing sources never resolve - each call path.


@pytest.mark.parametrize("bad", BAD_SOURCES)
def test_search_and_score_path_rejects_unknown_source(tmp_path, bad):
    orch = _stub_orchestrator(_gate_registry(), tmp_path / "staging")
    with pytest.raises(OrchestrationError, match="Unknown source|No working source"):
        orch._strategy(bad)
    assert orch._strategies["soulseek"] is not None
    assert orch._source_enabled(bad) is False


@pytest.mark.parametrize("bad", BAD_SOURCES)
def test_enqueue_path_rejects_unknown_source(tmp_path, bad):
    orch = _stub_orchestrator(_gate_registry(), tmp_path / "staging")
    with pytest.raises(OrchestrationError, match="Unknown source|No working source"):
        orch._strategy(_task_ns(bad).source)
    assert orch._source_enabled(bad) is False


@pytest.mark.parametrize("bad", BAD_SOURCES)
def test_import_files_path_rejects_unknown_source(tmp_path, bad):
    orch = _stub_orchestrator(_gate_registry(), tmp_path / "staging")
    with pytest.raises(OrchestrationError, match="Unknown source|No working source"):
        orch._strategy(_task_ns(bad).source)
    assert orch._source_enabled(bad) is False


@pytest.mark.parametrize("bad", BAD_SOURCES)
def test_candidate_identity_path_rejects_unknown_source(tmp_path, bad):
    orch = _stub_orchestrator(_gate_registry(), tmp_path / "staging")
    with pytest.raises(OrchestrationError, match="Unknown source|No working source"):
        orch._candidate_source_identity(_candidate_ns(bad))
    assert orch._source_enabled(bad) is False


@pytest.mark.parametrize("bad", BAD_SOURCES)
def test_download_client_for_path_rejects_unknown_source(tmp_path, bad):
    orch = _stub_orchestrator(_gate_registry(), tmp_path / "staging")
    with pytest.raises(OrchestrationError, match="Unknown source|No working source"):
        orch._download_client_for(_task_ns(bad))
    with pytest.raises(OrchestrationError, match="Unknown source|No working source"):
        orch._client_for_source(bad)
    assert orch._source_enabled(bad) is False


@pytest.mark.parametrize("bad", BAD_SOURCES)
def test_queued_timeout_poll_path_rejects_unknown_source(tmp_path, bad):
    """The poll loop reads ``applies_queued_timeout`` off the strategy: an
    unknown source must raise before any timeout policy is read."""
    orch = _stub_orchestrator(_gate_registry(), tmp_path / "staging")
    with pytest.raises(OrchestrationError, match="Unknown source|No working source"):
        orch._strategy(bad).applies_queued_timeout
    assert orch._source_enabled(bad) is False


@pytest.mark.parametrize("bad", BAD_SOURCES)
def test_failover_fault_policy_path_rejects_unknown_source(tmp_path, bad):
    """Failover reads the fault policy off the strategy: unknown sources raise
    instead of inheriting the soulseek/slskd policy."""
    orch = _stub_orchestrator(_gate_registry(), tmp_path / "staging")
    with pytest.raises(OrchestrationError, match="Unknown source|No working source"):
        orch._strategy(bad).has_local_disk_faults
    with pytest.raises(OrchestrationError, match="Unknown source|No working source"):
        orch._strategy(bad).local_fault_message(False)
    assert orch._source_enabled(bad) is False


def test_known_plugin_key_resolves_its_own_strategy_not_soulseek(tmp_path):
    orch = _stub_orchestrator(_gate_registry(), tmp_path / "staging")
    strategy = orch._strategy(GOOD_KEY)
    assert strategy.name == GOOD_KEY
    assert strategy is not orch._strategies["soulseek"]
    assert orch._source_enabled(GOOD_KEY) is True
    assert orch._client_for_source(GOOD_KEY) == GOOD_KEY


# (b) plugin attempts/manifests/handles carry the plugin key.


@pytest.mark.asyncio
async def test_plugin_attempt_rows_never_carry_soulseek_source(tmp_path):
    store = AsyncMock()
    store.create_download_attempt.return_value = SimpleNamespace(id="att-1")
    client = AsyncMock()
    client.enqueue.return_value = TaskHandle(source=GOOD_KEY, job_name="j1")
    strategy = PluginSourceStrategy(
        indexer=MagicMock(),
        scorer=MagicMock(),
        track_matcher=MagicMock(),
        client=client,
        store=store,
        file_processor=MagicMock(),
        staging=tmp_path / "staging",
        manifest_codec=ManifestCodec(),
        naming_template=_TEMPLATE,
        source_key=GOOD_KEY,
        display_name="Good Plug",
    )
    assert strategy.name == GOOD_KEY
    candidate = ScoredCandidate(
        source=GOOD_KEY,
        plugin_release=_plugin_release(),
        files=[],
        coherence=0.9,
        file_confidence=0.9,
        final_score=0.9,
        tier="auto",
    )
    task = SimpleNamespace(
        id="t1",
        download_type="album",
        track_count=12,
        track_title=None,
        track_duration_seconds=None,
        release_mbid=None,
        release_group_mbid="",
        origin="user",
        artist_mbid=None,
        artist_name="A",
        album_title="B",
        year=None,
        candidate_index=0,
    )
    await strategy.enqueue(task, candidate, strict_track_duration=True)

    attempt_kwargs = store.create_download_attempt.await_args.kwargs
    assert attempt_kwargs["source"] == GOOD_KEY
    assert attempt_kwargs["source"] != "soulseek"
    assert attempt_kwargs["handle"].source == GOOD_KEY
    request = client.enqueue.await_args.args[0]
    assert request.source == GOOD_KEY
    manifest = ManifestCodec().decode(
        (tmp_path / "staging" / "t1" / "manifest.json").read_bytes()
    )
    assert manifest.handle.source == GOOD_KEY
    assert manifest.handle.source != "soulseek"
    identity = strategy.candidate_identity(candidate)
    assert identity == plugin_identity(GOOD_KEY, "tok-1")
    assert "soulseek" not in identity


# (c) one broken plugin indexer drops only its group.


@pytest.mark.asyncio
async def test_run_search_drops_only_the_broken_plugin_group():
    good = _FakePlugin(
        "goodplug", instance=_FakeInstance(releases=[_indexer_hit(GOOD_KEY)])
    )
    bad = _FakePlugin(
        "badplug",
        instance=_FakeInstance(releases=[_indexer_hit("plugin:badplug", "Bad Release")]),
    )
    service, store = _service(
        registry=_registry(good, bad), scorer=_rank_good_only(GOOD_KEY)
    )
    await service._run_search(
        "job1", "A", "B", None, None, None, snapshot=service._search_snapshot()
    )
    pooled = store.set_search_job_candidates.await_args.args[1]
    assert [c.source for c in pooled] == [GOOD_KEY]
    store.update_search_job_status.assert_awaited_with("job1", "completed")


@pytest.mark.asyncio
async def test_scout_album_drops_raising_indexer_keeps_good_group():
    good = _FakePlugin(
        "goodplug", instance=_FakeInstance(releases=[_indexer_hit(GOOD_KEY)])
    )
    bad = _FakePlugin("badplug", instance=_FakeInstance(exc=RuntimeError("boom")))
    service, _ = _service(registry=_registry(good, bad), scorer=_rank_good_only(GOOD_KEY))
    found = await service.scout_album("A", "B")
    assert [c.source for c in found] == [GOOD_KEY]


# (d) plugin results surface grouped under the plugin key.


@pytest.mark.asyncio
async def test_search_plugin_groups_candidates_under_plugin_key():
    good = _FakePlugin(
        "goodplug", instance=_FakeInstance(releases=[_indexer_hit(GOOD_KEY)])
    )
    registry = _registry(good)
    service, _ = _service(registry=registry, scorer=None)
    # Real scorer (built inside _search_plugin from the service store): proves
    # the source_key plumbing, not a stub echo.
    assert service._plugin_scorer is None
    found = await service._search_plugin(
        TargetAlbum(artist_name="A", album_title="B"),
        registry.spec_for(GOOD_KEY),
        snapshot=service._search_snapshot(),
    )
    assert found, "expected the real scorer to keep the 0.95 lossless release"
    assert {c.source for c in found} == {GOOD_KEY}
    assert all(c.plugin_release is not None for c in found)
    assert service._client_for_source(GOOD_KEY) == GOOD_KEY
    with pytest.raises(Exception, match="Unknown source"):
        service._client_for_source("plugin:ghost")

# (e) usenet-targeting specs pool via CompositeIndexer: no direct strategy.


def _usenet_target_plugin():
    plug = _FakePlugin("usenetplug", instance=_FakeInstance(configured=True))
    plug.manifest.capability_configs = [
        SimpleNamespace(id="indexer", target_source="usenet")
    ]
    return plug


def test_usenet_target_spec_yields_no_direct_strategy(tmp_path):
    registry = _registry(_usenet_target_plugin())
    spec = registry.spec_for("plugin:usenetplug")
    assert spec is not None and spec.target_source == "usenet"
    assert spec.has_client is True

    orch = _stub_orchestrator(registry, tmp_path / "staging")
    assert "plugin:usenetplug" not in orch._strategies
    assert orch._source_enabled("plugin:usenetplug") is False
    assert orch._ensure_plugin_strategy("plugin:usenetplug") is None
    with pytest.raises(OrchestrationError, match="Unknown source"):
        orch._strategy("plugin:usenetplug")


# (f) folder-mode import quarantines failed releases like files mode does.


def _folder_strategy(tmp_path, *, store, client, file_processor):
    return PluginSourceStrategy(
        indexer=MagicMock(),
        scorer=MagicMock(),
        track_matcher=MagicMock(),
        client=client,
        store=store,
        file_processor=file_processor,
        staging=tmp_path / "staging",
        manifest_codec=ManifestCodec(),
        naming_template=_TEMPLATE,
        source_key=GOOD_KEY,
        display_name="Good Plug",
    )


def _folder_task():
    return SimpleNamespace(
        id="task-1", release_group_mbid="rg-1", search_job_id="job-1", candidate_index=0
    )


def _folder_manifest():
    return SimpleNamespace(
        handle=SimpleNamespace(job_name="j1"), target_files=[], task_id="task-1"
    )


@pytest.mark.asyncio
async def test_folder_import_quarantines_failed_release(tmp_path):
    from services.native.file_processor import FileFailure, ProcessResult

    store = AsyncMock()
    store.get_search_job_candidates.return_value = [
        SimpleNamespace(source=GOOD_KEY, plugin_release=_plugin_release())
    ]
    client = AsyncMock()
    client.list_completed_files.return_value = [tmp_path / "track.flac"]
    file_processor = AsyncMock()
    file_processor.process_downloaded_folder.return_value = ProcessResult(
        succeeded=[],
        failed=[FileFailure(filename="track.flac", reason="tag_mismatch")],
    )
    strategy = _folder_strategy(
        tmp_path, store=store, client=client, file_processor=file_processor
    )

    result, enumerated = await strategy.import_files(
        _folder_task(), _folder_manifest(), completed=True
    )

    assert enumerated == 1
    assert result.succeeded == []
    store.record_quarantine.assert_awaited_once()
    kwargs = store.record_quarantine.await_args.kwargs
    assert kwargs["source"] == GOOD_KEY
    assert kwargs["identity"] == plugin_identity(GOOD_KEY, "tok-1")
    assert kwargs["reason"] == "verify_failed"
    assert kwargs["release_group_mbid"] == "rg-1"
    store.set_final_path.assert_not_awaited()


@pytest.mark.asyncio
async def test_folder_import_skips_quarantine_for_local_faults(tmp_path):
    from services.native.file_processor import FileFailure, ProcessResult

    store = AsyncMock()
    store.get_search_job_candidates.return_value = [
        SimpleNamespace(source=GOOD_KEY, plugin_release=_plugin_release())
    ]
    client = AsyncMock()
    client.list_completed_files.return_value = [tmp_path / "track.flac"]
    file_processor = AsyncMock()
    file_processor.process_downloaded_folder.return_value = ProcessResult(
        succeeded=[],
        failed=[FileFailure(filename="track.flac", reason="wrong_track")],
    )
    strategy = _folder_strategy(
        tmp_path, store=store, client=client, file_processor=file_processor
    )

    await strategy.import_files(_folder_task(), _folder_manifest(), completed=True)

    store.record_quarantine.assert_not_awaited()


@pytest.mark.asyncio
async def test_folder_import_never_quarantines_target_occupied(tmp_path):
    """#418: a target_occupied collision is a local fault (a stray file at the
    destination), never proof the release is bad - the release must not be
    quarantined."""
    from services.native.file_processor import (
        QUARANTINE_REASONS,
        TARGET_OCCUPIED,
        FileFailure,
        ProcessResult,
    )

    assert TARGET_OCCUPIED not in QUARANTINE_REASONS
    store = AsyncMock()
    store.get_search_job_candidates.return_value = [
        SimpleNamespace(source=GOOD_KEY, plugin_release=_plugin_release())
    ]
    client = AsyncMock()
    client.list_completed_files.return_value = [tmp_path / "track.flac"]
    file_processor = AsyncMock()
    file_processor.process_downloaded_folder.return_value = ProcessResult(
        succeeded=[],
        failed=[FileFailure(filename="track.flac", reason=TARGET_OCCUPIED)],
    )
    strategy = _folder_strategy(
        tmp_path, store=store, client=client, file_processor=file_processor
    )

    result, enumerated = await strategy.import_files(
        _folder_task(), _folder_manifest(), completed=True
    )

    assert enumerated == 1
    assert [f.reason for f in result.failed] == [TARGET_OCCUPIED]
    store.record_quarantine.assert_not_awaited()
    store.set_final_path.assert_not_awaited()


@pytest.mark.asyncio
async def test_files_import_never_quarantines_target_occupied(tmp_path):
    """#418 files-mode equivalent: a per-file target_occupied collision never
    quarantines the plugin release."""
    from services.native.file_processor import (
        QUARANTINE_REASONS,
        TARGET_OCCUPIED,
        FileFailure,
        ProcessResult,
    )

    assert TARGET_OCCUPIED not in QUARANTINE_REASONS
    store = AsyncMock()
    store.get_search_job_candidates.return_value = [
        SimpleNamespace(source=GOOD_KEY, plugin_release=_plugin_release())
    ]
    file_processor = AsyncMock()
    file_processor.process_downloaded.return_value = ProcessResult(
        succeeded=[],
        failed=[FileFailure(filename="track.flac", reason=TARGET_OCCUPIED)],
    )
    strategy = _folder_strategy(
        tmp_path, store=store, client=AsyncMock(), file_processor=file_processor
    )
    manifest = SimpleNamespace(
        handle=SimpleNamespace(job_name="j1"),
        target_files=[SimpleNamespace(filename="track.flac")],
        task_id="task-1",
    )

    result, _enumerated = await strategy.import_files(
        _folder_task(), manifest, completed=True
    )

    assert [f.reason for f in result.failed] == [TARGET_OCCUPIED]
    store.record_quarantine.assert_not_awaited()
    store.set_final_path.assert_not_awaited()
