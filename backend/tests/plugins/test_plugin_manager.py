"""PluginManager: built-in registration, external discovery (single .py module and
package), entry-point discovery, api_version rejection, quarantine of broken
modules, duplicate-id handling, enable/disable + settings persistence with
secret masking."""

import textwrap
from pathlib import Path

import pytest

from plugins.base import API_VERSION, PLUGIN_SECRET_MASK
from plugins.manager import PluginManager

_GOOD_PLUGIN = textwrap.dedent(
    '''
    from plugins.base import AcquisitionPlugin, SettingsField, TestResult


    class GoodPlugin(AcquisitionPlugin):
        id = "good_plugin"
        name = "Good Plugin"
        version = "2.3.4"

        def settings_schema(self):
            return [
                SettingsField(key="endpoint", type="str", label="Endpoint", default="http://x"),
                SettingsField(key="token", type="secret", label="Token", default=""),
                SettingsField(key="limit", type="int", label="Limit", default=5),
                SettingsField(key="fast", type="bool", label="Fast", default=True),
            ]

        def configure(self, settings):
            self.applied = dict(settings)

        async def test_connection(self, settings=None):
            values = settings or getattr(self, "applied", {})
            return TestResult(ok=bool(values.get("endpoint")), message="tested", version="2.3.4")

        async def search(self, request):
            return []

        async def enqueue(self, request):
            raise NotImplementedError

        async def get_status(self, handle):
            raise NotImplementedError

        async def cancel(self, handle):
            return True

        async def completed_path(self, handle):
            return []
    '''
)


class _FakePrefs:
    """Dict-backed stand-in for the two PreferencesService plugin methods."""

    def __init__(self):
        self.store: dict[str, dict] = {}

    def get_plugins_config(self) -> dict:
        return dict(self.store)

    def save_plugin_config_raw(self, plugin_id: str, cfg: dict) -> None:
        self.store[plugin_id] = cfg


def _manager(tmp_path: Path) -> PluginManager:
    return PluginManager(preferences=_FakePrefs(), external_dir=tmp_path / "plugins")


def _external_dir(tmp_path: Path) -> Path:
    d = tmp_path / "plugins"
    d.mkdir(parents=True, exist_ok=True)
    return d


# ------------------------------------------------------------- built-ins


def test_builtins_are_registered(tmp_path):
    manager = _manager(tmp_path)
    records = {r.id: r for r in manager.list_records()}
    assert "soulseek" in records and records["soulseek"].builtin
    assert "usenet" in records and records["usenet"].builtin
    assert records["soulseek"].loaded and records["usenet"].loaded
    assert manager.get_plugin("soulseek") is not None
    assert manager.get_plugin("usenet") is not None


# ------------------------------------------------------- external discovery


def test_external_single_module_loads(tmp_path):
    (_external_dir(tmp_path) / "good_plugin.py").write_text(_GOOD_PLUGIN, encoding="utf-8")
    manager = _manager(tmp_path)
    record = manager.get_record("good_plugin")
    assert record is not None and record.loaded
    assert record.version == "2.3.4"
    assert not record.builtin
    # configure ran at load with schema defaults
    assert manager.get_plugin("good_plugin").applied["endpoint"] == "http://x"
    assert manager.get_plugin("good_plugin").applied["limit"] == 5


def test_external_package_loads(tmp_path):
    pkg = _external_dir(tmp_path) / "pkg_plugin"
    pkg.mkdir()
    (pkg / "__init__.py").write_text(
        _GOOD_PLUGIN.replace("good_plugin", "pkg_plugin").replace("GoodPlugin", "PkgPlugin"),
        encoding="utf-8",
    )
    manager = _manager(tmp_path)
    record = manager.get_record("pkg_plugin")
    assert record is not None and record.loaded


def test_broken_external_module_is_quarantined_not_fatal(tmp_path):
    ext = _external_dir(tmp_path)
    (ext / "broken.py").write_text("raise RuntimeError('boom on import')", encoding="utf-8")
    (ext / "good_plugin.py").write_text(_GOOD_PLUGIN, encoding="utf-8")
    manager = _manager(tmp_path)
    # the good one still loads; the broken one is quarantined with its error
    assert manager.get_plugin("good_plugin") is not None
    broken = manager.get_record("broken")
    assert broken is not None and not broken.loaded
    assert "boom on import" in (broken.error or "")


def test_module_without_plugin_class_is_quarantined(tmp_path):
    (_external_dir(tmp_path) / "empty.py").write_text("X = 1\n", encoding="utf-8")
    manager = _manager(tmp_path)
    record = manager.get_record("empty")
    assert record is not None and not record.loaded
    assert "no AcquisitionPlugin subclass" in (record.error or "")


def test_wrong_api_version_is_rejected(tmp_path):
    bad = _GOOD_PLUGIN.replace(
        'version = "2.3.4"', f'version = "2.3.4"\n    api_version = {API_VERSION + 1}'
    ).replace("good_plugin", "future_plugin")
    (_external_dir(tmp_path) / "future_plugin.py").write_text(bad, encoding="utf-8")
    manager = _manager(tmp_path)
    record = manager.get_record("future_plugin")
    assert record is not None and not record.loaded
    assert f"api_version {API_VERSION + 1}" in (record.error or "")
    assert manager.get_plugin("future_plugin") is None


def test_duplicate_id_keeps_first_and_quarantines_second(tmp_path):
    ext = _external_dir(tmp_path)
    dup = _GOOD_PLUGIN.replace("good_plugin", "soulseek")  # collides with the built-in
    (ext / "soulseek_clone.py").write_text(dup, encoding="utf-8")
    manager = _manager(tmp_path)
    record = manager.get_record("soulseek")
    assert record is not None and record.builtin  # the FIRST registration won
    duplicates = [r for r in manager.list_records() if "duplicate plugin id" in (r.error or "")]
    assert len(duplicates) == 1


def test_plugin_raising_in_init_is_quarantined(tmp_path):
    src = _GOOD_PLUGIN.replace("good_plugin", "init_boom") + textwrap.dedent(
        """
        class InitBoom(GoodPlugin):
            id = "boom_plugin"

            def __init__(self):
                raise RuntimeError("boom in init")
        """
    )
    (_external_dir(tmp_path) / "init_boom.py").write_text(src, encoding="utf-8")
    manager = _manager(tmp_path)
    record = manager.get_record("boom_plugin")
    assert record is not None and not record.loaded
    assert "boom in init" in (record.error or "")
    # the sibling class in the same module still loads
    assert manager.get_plugin("init_boom") is not None


# --------------------------------------------------------- entry points


def test_entry_point_discovery(tmp_path, monkeypatch):
    import importlib.metadata

    from plugins.base import AcquisitionPlugin, TestResult

    class EpPlugin(AcquisitionPlugin):
        id = "ep_plugin"
        name = "EP Plugin"
        version = "0.1.0"

        def settings_schema(self):
            return []

        def configure(self, settings):
            return None

        async def test_connection(self, settings=None):
            return TestResult(ok=True)

        async def search(self, request):
            return []

        async def enqueue(self, request):
            raise NotImplementedError

        async def get_status(self, handle):
            raise NotImplementedError

        async def cancel(self, handle):
            return True

        async def completed_path(self, handle):
            return []

    class _FakeEp:
        name = "ep_plugin"
        value = "some_dist:EpPlugin"

        @staticmethod
        def load():
            return EpPlugin

    real_entry_points = importlib.metadata.entry_points

    def fake_entry_points(**kwargs):
        if kwargs.get("group") == "droppedneedle.plugins":
            return [_FakeEp()]
        return real_entry_points(**kwargs)

    monkeypatch.setattr(importlib.metadata, "entry_points", fake_entry_points)
    manager = _manager(tmp_path)
    record = manager.get_record("ep_plugin")
    assert record is not None and record.loaded and record.source == "entry_point"


# --------------------------------------------- enablement + settings persistence


def test_external_enable_disable_persists(tmp_path):
    (_external_dir(tmp_path) / "good_plugin.py").write_text(_GOOD_PLUGIN, encoding="utf-8")
    prefs = _FakePrefs()
    manager = PluginManager(preferences=prefs, external_dir=tmp_path / "plugins")
    assert manager.is_enabled("good_plugin") is True  # default on
    manager.set_enabled("good_plugin", False)
    assert manager.is_enabled("good_plugin") is False
    assert prefs.store["good_plugin"]["enabled"] is False
    # a fresh manager over the same store sees the persisted flag
    manager2 = PluginManager(preferences=prefs, external_dir=tmp_path / "plugins")
    assert manager2.is_enabled("good_plugin") is False


def test_settings_roundtrip_masks_and_preserves_secret(tmp_path):
    (_external_dir(tmp_path) / "good_plugin.py").write_text(_GOOD_PLUGIN, encoding="utf-8")
    prefs = _FakePrefs()
    manager = PluginManager(preferences=prefs, external_dir=tmp_path / "plugins")

    manager.save_settings(
        "good_plugin",
        {"endpoint": "http://real", "token": "s3cret", "limit": 9, "fast": False},
    )
    plugin = manager.get_plugin("good_plugin")
    assert plugin.applied == {
        "endpoint": "http://real",
        "token": "s3cret",
        "limit": 9,
        "fast": False,
    }
    # at rest the secret is encrypted, never plaintext
    assert prefs.store["good_plugin"]["settings"]["token"] != "s3cret"

    masked = manager.get_settings_values("good_plugin")
    assert masked["token"] == PLUGIN_SECRET_MASK
    assert masked["endpoint"] == "http://real"

    # saving the mask back preserves the stored secret
    manager.save_settings("good_plugin", {"endpoint": "http://real2", "token": PLUGIN_SECRET_MASK})
    assert plugin.applied["token"] == "s3cret"
    assert plugin.applied["endpoint"] == "http://real2"
    # unchanged keys survive a partial save
    assert plugin.applied["limit"] == 9

    raw = manager.get_settings_values("good_plugin", raw=True)
    assert raw["token"] == "s3cret"


@pytest.mark.asyncio
async def test_test_connection_uses_submitted_values_without_saving(tmp_path):
    (_external_dir(tmp_path) / "good_plugin.py").write_text(_GOOD_PLUGIN, encoding="utf-8")
    prefs = _FakePrefs()
    manager = PluginManager(preferences=prefs, external_dir=tmp_path / "plugins")
    result = await manager.test("good_plugin", {"endpoint": ""})
    assert result.ok is False
    result = await manager.test("good_plugin", {"endpoint": "http://y"})
    assert result.ok is True and result.version == "2.3.4"
    # nothing persisted by testing
    assert "settings" not in prefs.store.get("good_plugin", {})


@pytest.mark.asyncio
async def test_test_unknown_plugin_is_a_result_not_an_error(tmp_path):
    manager = _manager(tmp_path)
    result = await manager.test("nope")
    assert result.ok is False and "unknown plugin" in result.message


def test_missing_external_dir_is_fine(tmp_path):
    manager = PluginManager(preferences=_FakePrefs(), external_dir=tmp_path / "does-not-exist")
    assert manager.get_plugin("soulseek") is not None
