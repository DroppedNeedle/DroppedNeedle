"""PluginHost + manifest: validation, the disabled-by-default trust model,
failure isolation, capability dispatch, and the shipped example plugins as
executable fixtures."""

import asyncio
from pathlib import Path

import pytest

from api.v1.schemas.settings import PluginConfig
from infrastructure.plugins.host import PluginHost
from infrastructure.plugins.manifest import ManifestError, load_manifest
from infrastructure.plugins.protocols import PluginContext, ScrobbleEvent

EXAMPLES = Path(__file__).parent.parent.parent.parent / "examples" / "plugins"

VALID_MANIFEST = """
[plugin]
name = "test-plugin"
version = "1.0.0"
api_version = 0
entrypoint = "plugin:TestPlugin"
capabilities = ["scrobbler"]
"""

SCROBBLER_CODE = """
SEEN = []

class TestPlugin:
    def __init__(self, context):
        self.ctx = context

    async def on_scrobble(self, event):
        SEEN.append(event.track)
"""


class FakePrefs:
    def __init__(self) -> None:
        self.configs: dict[str, PluginConfig] = {}

    def get_plugin_config(self, name: str) -> PluginConfig:
        return self.configs.get(name, PluginConfig())

    def enable(self, name: str, settings: dict | None = None) -> None:
        self.configs[name] = PluginConfig(enabled=True, settings=settings or {})


def _write_plugin(root: Path, name: str, manifest: str, code: str) -> Path:
    plugin_dir = root / name
    plugin_dir.mkdir(parents=True)
    (plugin_dir / "plugin.toml").write_text(manifest)
    (plugin_dir / "plugin.py").write_text(code)
    return plugin_dir


# -- manifest validation --


def test_manifest_rejects_wrong_api_version(tmp_path):
    _write_plugin(tmp_path, "p", VALID_MANIFEST.replace("api_version = 0", "api_version = 99"), "")
    with pytest.raises(ManifestError, match="api_version"):
        load_manifest(tmp_path / "p")


def test_manifest_rejects_unknown_capability(tmp_path):
    bad = VALID_MANIFEST.replace('["scrobbler"]', '["mind_reader"]')
    _write_plugin(tmp_path, "p", bad, "")
    with pytest.raises(ManifestError, match="unknown capabilities"):
        load_manifest(tmp_path / "p")


def test_manifest_accepts_reserved_capabilities(tmp_path):
    reserved = VALID_MANIFEST.replace('["scrobbler"]', '["metadata_provider"]')
    _write_plugin(tmp_path, "p", reserved, "")
    manifest = load_manifest(tmp_path / "p")
    assert manifest.capabilities == ["metadata_provider"]


def test_manifest_requires_entrypoint_shape(tmp_path):
    bad = VALID_MANIFEST.replace('"plugin:TestPlugin"', '"plugin.TestPlugin"')
    _write_plugin(tmp_path, "p", bad, "")
    with pytest.raises(ManifestError, match="entrypoint"):
        load_manifest(tmp_path / "p")


# -- trust model --


def test_disabled_plugin_runs_no_code(tmp_path):
    """Dropping a folder in must be inert: the module is never imported until
    an admin enables the plugin."""
    booby_trap = tmp_path / "boom.txt"
    code = f"open({str(booby_trap)!r}, 'w').write('ran')\n\nclass TestPlugin:\n    def __init__(self, ctx): ...\n"
    _write_plugin(tmp_path, "test-plugin", VALID_MANIFEST, code)
    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())

    host.load_all()

    assert not booby_trap.exists()
    plugin = host.get("test-plugin")
    assert plugin is not None and plugin.enabled is False and plugin.instance is None


def test_enabled_scrobbler_loads_and_dispatches(tmp_path):
    prefs = FakePrefs()
    prefs.enable("test-plugin")
    _write_plugin(tmp_path, "test-plugin", VALID_MANIFEST, SCROBBLER_CODE)
    host = PluginHost(plugins_dir=tmp_path, preferences_service=prefs)
    host.load_all()

    plugin = host.get("test-plugin")
    assert plugin is not None and plugin.active_capabilities == ["scrobbler"]

    asyncio.run(host.dispatch_scrobble(ScrobbleEvent(artist="A", track="Song")))
    module = type(plugin.instance).__module__
    import sys

    assert sys.modules[module].SEEN == ["Song"]


def test_broken_plugin_is_isolated(tmp_path):
    prefs = FakePrefs()
    prefs.enable("test-plugin")
    _write_plugin(tmp_path, "test-plugin", VALID_MANIFEST, "raise RuntimeError('boom')")
    host = PluginHost(plugins_dir=tmp_path, preferences_service=prefs)

    host.load_all()  # must not raise

    plugin = host.get("test-plugin")
    assert plugin is not None
    assert plugin.enabled is False
    assert "Failed to load" in (plugin.error or "")


def test_invalid_manifest_is_surfaced_not_fatal(tmp_path):
    plugin_dir = tmp_path / "broken"
    plugin_dir.mkdir()
    (plugin_dir / "plugin.toml").write_text("not [valid toml")
    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())

    host.load_all()

    plugin = host.get("broken")
    assert plugin is not None and plugin.error


def test_one_crashing_scrobbler_does_not_stop_the_next(tmp_path):
    prefs = FakePrefs()
    prefs.enable("a-crasher")
    prefs.enable("b-worker")
    crasher = VALID_MANIFEST.replace('"test-plugin"', '"a-crasher"')
    worker = VALID_MANIFEST.replace('"test-plugin"', '"b-worker"')
    _write_plugin(
        tmp_path, "a-crasher", crasher,
        "class TestPlugin:\n    def __init__(self, ctx): ...\n"
        "    async def on_scrobble(self, event):\n        raise RuntimeError('boom')\n",
    )
    _write_plugin(tmp_path, "b-worker", worker, SCROBBLER_CODE)
    host = PluginHost(plugins_dir=tmp_path, preferences_service=prefs)
    host.load_all()

    asyncio.run(host.dispatch_scrobble(ScrobbleEvent(artist="A", track="Song")))

    import sys

    worker_plugin = host.get("b-worker")
    module = type(worker_plugin.instance).__module__
    assert sys.modules[module].SEEN == ["Song"]


def test_require_source_rejects_non_source_plugins(tmp_path):
    from core.exceptions import ResourceNotFoundError

    prefs = FakePrefs()
    prefs.enable("test-plugin")
    _write_plugin(tmp_path, "test-plugin", VALID_MANIFEST, SCROBBLER_CODE)
    host = PluginHost(plugins_dir=tmp_path, preferences_service=prefs)
    host.load_all()

    with pytest.raises(ResourceNotFoundError):
        host.require_source("test-plugin")
    with pytest.raises(ResourceNotFoundError):
        host.require_source("does-not-exist")


# -- the shipped examples are executable fixtures --


def test_example_plugins_load_with_their_capabilities():
    prefs = FakePrefs()
    prefs.enable("webhook-scrobbler")
    prefs.enable("lma-source")
    host = PluginHost(plugins_dir=EXAMPLES, preferences_service=prefs)
    host.load_all()

    scrobbler = host.get("webhook-scrobbler")
    assert scrobbler is not None and scrobbler.active_capabilities == ["scrobbler"]
    source = host.get("lma-source")
    assert source is not None and source.active_capabilities == ["audio_source"]
    assert [p.manifest.name for p in host.sources()] == ["lma-source"]


@pytest.mark.asyncio
async def test_webhook_scrobbler_posts_the_play():
    from unittest.mock import AsyncMock

    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "webhook_scrobbler_test", EXAMPLES / "webhook-scrobbler" / "plugin.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    http = AsyncMock()
    http.post = AsyncMock(return_value=type("R", (), {"status_code": 200})())
    context = PluginContext(
        plugin_name="webhook-scrobbler",
        settings=lambda: {"webhook_url": "https://hooks.example/x"},
        http=http,
    )
    plugin = module.WebhookScrobbler(context)

    await plugin.on_scrobble(ScrobbleEvent(artist="A", track="T", timestamp=5))

    http.post.assert_awaited_once()
    assert http.post.await_args.kwargs["json"]["track"] == "T"


# -- install from GitHub --


def _github_zip(root: str = "repo-main", manifest: str = VALID_MANIFEST, extra: dict | None = None) -> bytes:
    import io
    import zipfile

    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as zf:
        zf.writestr(f"{root}/plugin.toml", manifest)
        zf.writestr(f"{root}/plugin.py", SCROBBLER_CODE)
        for name, content in (extra or {}).items():
            zf.writestr(f"{root}/{name}", content)
    return buffer.getvalue()


class _FakeResponse:
    def __init__(self, status_code: int, content: bytes = b"", headers: dict | None = None) -> None:
        self.status_code = status_code
        self.content = content
        self.headers = headers or {}


def _fake_http(responses: dict[str, _FakeResponse]):
    from unittest.mock import AsyncMock

    http = AsyncMock()

    async def _get(url: str, **_kwargs):
        for fragment, response in responses.items():
            if fragment in url:
                return response
        return _FakeResponse(404)

    http.get = AsyncMock(side_effect=_get)
    return http


@pytest.mark.asyncio
async def test_install_from_github_lands_disabled(tmp_path):
    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())
    http = _fake_http({"heads/main": _FakeResponse(200, _github_zip())})

    name = await host.install_from_github("https://github.com/owner/repo", http)

    assert name == "test-plugin"
    assert (tmp_path / "test-plugin" / "plugin.toml").is_file()
    plugin = host.get("test-plugin")
    # installed code is stored, never run: enabling stays the admin's decision
    assert plugin is not None and plugin.enabled is False and plugin.instance is None


@pytest.mark.asyncio
async def test_install_falls_back_to_master_branch(tmp_path):
    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())
    http = _fake_http({"heads/master": _FakeResponse(200, _github_zip("repo-master"))})

    name = await host.install_from_github("https://github.com/owner/repo", http)
    assert name == "test-plugin"


@pytest.mark.asyncio
async def test_install_rejects_non_github_urls(tmp_path):
    from infrastructure.plugins.host import PluginInstallError

    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())
    for bad in (
        "https://evil.example.com/owner/repo",
        "http://github.com/owner/repo",
        "https://github.com/owner",
        "not a url",
    ):
        with pytest.raises(PluginInstallError):
            await host.install_from_github(bad, _fake_http({}))


@pytest.mark.asyncio
async def test_install_rejects_traversal_components_in_the_url(tmp_path):
    """'..' survives the URL character classes; it must not reach codeload."""
    from infrastructure.plugins.host import PluginInstallError

    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())
    http = _fake_http({})
    for bad in (
        "https://github.com/owner/..",
        "https://github.com/../repo",
        "https://github.com/owner/repo/tree/../../etc",
    ):
        with pytest.raises(PluginInstallError):
            await host.install_from_github(bad, http)
    http.get.assert_not_awaited()


@pytest.mark.asyncio
async def test_install_refuses_an_oversized_repo_before_buffering(tmp_path):
    from infrastructure.plugins.host import PluginInstallError

    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())
    http = _fake_http(
        {"heads/main": _FakeResponse(200, b"x", headers={"content-length": str(10**12)})}
    )

    with pytest.raises(PluginInstallError, match="too large"):
        await host.install_from_github("https://github.com/owner/repo", http)


@pytest.mark.asyncio
async def test_install_rejects_repo_without_manifest(tmp_path):
    import io
    import zipfile

    from infrastructure.plugins.host import PluginInstallError

    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as zf:
        zf.writestr("repo-main/README.md", "hi")
    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())
    http = _fake_http({"heads/main": _FakeResponse(200, buffer.getvalue())})

    with pytest.raises(PluginInstallError, match="plugin.toml"):
        await host.install_from_github("https://github.com/owner/repo", http)
    assert list(tmp_path.iterdir()) == []


@pytest.mark.asyncio
async def test_install_rejects_invalid_manifest_and_leaves_no_staging(tmp_path):
    from infrastructure.plugins.host import PluginInstallError

    bad = VALID_MANIFEST.replace("api_version = 0", "api_version = 99")
    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())
    http = _fake_http({"heads/main": _FakeResponse(200, _github_zip(manifest=bad))})

    with pytest.raises(PluginInstallError, match="api_version"):
        await host.install_from_github("https://github.com/owner/repo", http)
    assert list(tmp_path.iterdir()) == []


@pytest.mark.asyncio
async def test_install_refuses_traversal_entries(tmp_path):
    import io
    import zipfile

    from infrastructure.plugins.host import PluginInstallError

    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as zf:
        zf.writestr("repo-main/plugin.toml", VALID_MANIFEST)
        zf.writestr("repo-main/../../escape.py", "pwned")
    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())
    http = _fake_http({"heads/main": _FakeResponse(200, buffer.getvalue())})

    with pytest.raises(PluginInstallError):
        await host.install_from_github("https://github.com/owner/repo", http)
    assert not (tmp_path.parent / "escape.py").exists()


@pytest.mark.asyncio
async def test_install_over_existing_replaces_it(tmp_path):
    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())
    http = _fake_http({"heads/main": _FakeResponse(200, _github_zip(extra={"old.txt": "v1"}))})
    await host.install_from_github("https://github.com/owner/repo", http)
    assert (tmp_path / "test-plugin" / "old.txt").is_file()

    http = _fake_http({"heads/main": _FakeResponse(200, _github_zip())})
    await host.install_from_github("https://github.com/owner/repo", http)
    assert not (tmp_path / "test-plugin" / "old.txt").exists()


def test_uninstall_removes_only_that_folder(tmp_path):
    prefs = FakePrefs()
    _write_plugin(tmp_path, "test-plugin", VALID_MANIFEST, SCROBBLER_CODE)
    keep = _write_plugin(
        tmp_path, "other", VALID_MANIFEST.replace('"test-plugin"', '"other"'), SCROBBLER_CODE
    )
    host = PluginHost(plugins_dir=tmp_path, preferences_service=prefs)
    host.load_all()

    host.uninstall("test-plugin")

    assert not (tmp_path / "test-plugin").exists()
    assert keep.exists()
    assert host.get("test-plugin") is None


def test_uninstall_unknown_plugin_raises(tmp_path):
    from core.exceptions import ResourceNotFoundError

    host = PluginHost(plugins_dir=tmp_path, preferences_service=FakePrefs())
    host.load_all()
    with pytest.raises(ResourceNotFoundError):
        host.uninstall("nope")
