"""Structural regression guard for the E10/E44 single-writer invariant.

E10 requires every audio mutation to funnel through the staged
``LibraryManagementPublisher`` chain, and E44 retains an explicit set of
user/admin removal features as the only non-staged filesystem writers outside
it. The identifier-absence checks in
``tests/services/native/test_library_management_mutation_boundary.py`` pin
retired names, but they cannot see a NEW writer under any fresh name: a scalar
mutator renamed tomorrow ships with zero resistance there.

These guards are structural (parsed ASTs, never greps - prose in a docstring
cannot read as code). Any new ``mutagen`` import, ``MutagenFile`` call, or
filesystem mutation primitive outside the sanctioned modules fails here, and
each allowlist doubles as living documentation of who owns that writer and
under which decision/gate.
"""

import ast
from pathlib import Path

_BACKEND = Path(__file__).resolve().parents[2]
_SKIP_DIRS = {".venv", "__pycache__"}

# Modules where importing mutagen is sanctioned. Everything else in the
# backend must read/write audio through the staged chain (E10).
_MUTAGEN_ALLOWLIST = {
    # The E10 mutation funnel itself: AudioMetadataEngine.apply/.restore_snapshot,
    # whose only production caller is the publisher's staging helpers.
    "infrastructure/audio/metadata_engine.py",
    # The staged byte-level writer (write_riff_info), staged-plan-only.
    "infrastructure/audio/metadata_writer.py",
    # Byte-level RIFF INFO reader/writer, called only inside staged application.
    "infrastructure/audio/riff_info.py",
    # AudioTagger is a format-dispatching READER (read_tags/read_cover_art); no save.
    "infrastructure/audio/tagger.py",
    # NativeLyricsService uses MutagenFile strictly as a lyrics reader.
    "services/compat/native_lyrics_service.py",
    # The independently-authored raw-mutagen fixture oracle (house test rule).
    "tests/fixtures/library/generate.py",
}

# Modules under services/ and api/ allowed to touch filesystem mutation
# primitives (os.replace/rename/remove/unlink/rmdir, shutil.*, Path.unlink /
# .write_bytes). Each entry names the decision or gate that owns the writes.
_FILESYSTEM_ALLOWLIST = {
    "services/library_service.py",  # E44 user/admin removal lane (_recycle_paths/_unlink_paths)
    "services/native/target_catalog_writer_service.py",  # E44 target-lane removal (remove_track/remove_album/_recycle_album)
    "services/native/recycle_bin.py",  # D4/D19 upgrade recycle bin + prune task
    "services/native/acquisition_cleanup_service.py",  # workspace cleanup, fingerprint re-verified before every unlink
    "services/native/download_orchestrator.py",  # download staging tree maintenance (shutil.rmtree of task staging)
    "services/native/download_service.py",  # E44 held-import deletion on album removal
    "services/native/drop_import_service.py",  # incoming/staging sweep (pre-publication staging root)
    "services/native/free_music_service.py",  # free-music destination staging/cleanup
    "services/native/library_filesystem_coordinator.py",  # rooted rename/unlink primitives behind the E28/E36 fence
    "services/native/library_management_publisher.py",  # the staged publisher itself
    "services/native/library_management_recovery_service.py",  # crash-recovery compensation
    "services/native/edition_conversion_service.py",  # edition-conversion bundle staging
    "services/native/file_processor.py",  # slskd download staging before the import lane
    "services/playlist_service.py",  # user-initiated playlist export (writes outside the library)
    "services/navidrome_playlist_export_service.py",  # .m3u8 export; writes/removes only marker-owned playlist files, never audio
    "services/local_files_service.py",  # album zip export via NamedTemporaryFile outside the library
    "services/cache_service.py",  # disposable cache-dir maintenance
    "services/preferences_service.py",  # config.json persistence
    "services/home/genre_artwork_service.py",  # genre artwork cache maintenance
    "services/native/target_application_lifecycle.py",  # target application lifecycle cleanup
    "services/native/acquisition/strategy.py",  # acquisition payload staging (.part files)
    "services/native/precache/audiodb_phase.py",  # precache writes into the disposable cache
    "api/v1/routes/import_drop.py",  # drop-upload staging into the disposable incoming dir
    "api/v1/routes/profile.py",  # avatar upload staging/removal
}

_OS_ATTRS = {"replace", "rename", "remove", "unlink", "rmdir"}
_SHUTIL_ATTRS = {"move", "copy", "copyfile", "copystat", "rmtree"}
_PATH_ATTRS = {"unlink", "write_bytes"}


def _walked_modules() -> list[Path]:
    """Every backend module except .venv/__pycache__; tests are excluded except
    the raw-mutagen fixture generator, which is itself allowlisted."""
    modules = []
    for path in _BACKEND.rglob("*.py"):
        if any(part in _SKIP_DIRS for part in path.parts):
            continue
        if "tests" in path.parts:
            continue
        modules.append(path)
    return modules


def _parse(path: Path) -> ast.Module:
    return ast.parse(
        path.read_text(encoding="utf-8", errors="ignore"), filename=str(path)
    )


def _imported_roots(node: ast.AST) -> list[str]:
    """Full imported module names bound by an import statement."""
    if isinstance(node, ast.Import):
        return [alias.name for alias in node.names]
    if isinstance(node, ast.ImportFrom):
        return [node.module or ""]
    return []


def _dotted_name(node: ast.AST) -> str | None:
    """``a.b.c`` for a Name/Attribute chain, else None."""
    parts: list[str] = []
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        parts.append(node.id)
        return ".".join(reversed(parts))
    return None


def _relative(path: Path) -> str:
    return path.relative_to(_BACKEND).as_posix()


def test_mutagen_is_imported_only_by_sanctioned_modules():
    offenders: list[str] = []
    for path in _walked_modules():
        rel = _relative(path)
        if rel in _MUTAGEN_ALLOWLIST:
            continue
        for node in ast.walk(_parse(path)):
            for name in _imported_roots(node):
                if name == "mutagen" or name.startswith("mutagen."):
                    offenders.append(f"{rel}:{node.lineno}: import {name}")

    assert offenders == [], (
        "E10 broken: mutagen was imported outside the staged audio "
        f"infrastructure. Every tag write must go through the "
        "LibraryManagementPublisher chain.\n  " + "\n  ".join(offenders)
    )


def test_mutagen_file_constructor_is_called_only_within_sanctioned_modules():
    """Covers dynamic dispatch: a module that never imports mutagen but calls
    ``MutagenFile(...)`` received it from somewhere - that is a shadow writer
    seam even when read-shaped."""
    offenders: list[str] = []
    for path in _walked_modules():
        if _relative(path) in _MUTAGEN_ALLOWLIST:
            continue
        for node in ast.walk(_parse(path)):
            if not isinstance(node, ast.Call):
                continue
            func = node.func
            named = (
                func.id if isinstance(func, ast.Name) else
                getattr(func, "attr", None) if isinstance(func, ast.Attribute) else None
            )
            if named == "MutagenFile":
                offenders.append(f"{_relative(path)}:{node.lineno}: MutagenFile(...)")

    assert offenders == [], (
        "E10 broken: MutagenFile was constructed outside the sanctioned "
        f"reader/writer modules.\n  " + "\n  ".join(offenders)
    )


def test_services_and_api_touch_no_filesystem_primitives_outside_the_allowlist():
    offenders: list[str] = []
    for path in _walked_modules():
        rel = _relative(path)
        if not (rel.startswith("services/") or rel.startswith("api/")):
            continue
        if rel in _FILESYSTEM_ALLOWLIST:
            continue
        for node in ast.walk(_parse(path)):
            if not isinstance(node, ast.Call) or not isinstance(
                node.func, ast.Attribute
            ):
                continue
            attr = node.func.attr
            receiver = _dotted_name(node.func.value)
            primitive = None
            if receiver == "os" and attr in _OS_ATTRS:
                primitive = f"os.{attr}"
            elif receiver == "shutil" and attr in _SHUTIL_ATTRS:
                primitive = f"shutil.{attr}"
            elif attr in _PATH_ATTRS:
                primitive = f".{attr}()"
            if primitive:
                offenders.append(f"{rel}:{node.lineno}: {primitive}")

    assert offenders == [], (
        "E44 boundary broken: a filesystem mutation primitive appeared outside "
        "the sanctioned writers. Add the module to _FILESYSTEM_ALLOWLIST ONLY "
        "with a comment naming the owning decision or gate.\n  "
        + "\n  ".join(offenders)
    )
