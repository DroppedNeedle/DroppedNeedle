"""Bounded offline compaction of artwork bytes stored inline in finished import rows.

Before #517 was fixed, automatic imports, edition conversions, baseline restores and
duplicate copies wrote the cover art as base64 into every plan item (twice) and into
the import bundle's request JSON. Rows written since keep the bytes once in the blob
store; this command rewrites the older terminal rows the same way:

- import bundles in `completed`, `resolved` or `rolled_back` take the persisted
  reference form (artwork and external cover payloads move to the blob store, the
  JSON keeps their SHA-256) and `request_hash` is recomputed with it;
- plan items of completed jobs lose the artwork bytes from the desired document,
  gain embedded artwork choices, and drop the catalog document when it equals the
  desired one, with the document hashes recomputed.

Rows in any other state, and rows it cannot prove safe to rewrite, are left untouched
and counted as skipped. A run is idempotent: rewritten rows no longer match.

Like the snapshot compaction, this is intentionally separate from application
startup. Apply mode requires an explicit isolated-copy confirmation, never runs VACUUM
and never replaces a database. Each batch is one transaction, so an interrupted run
leaves only whole rows rewritten.

Usage, with the application stopped and the database copied (for example with
`sqlite3 library.db ".backup copy.db"`). `--blob-root` may be an empty directory: only
the new blobs are written there, and its `objects/` folder is then copied into the
live blob store (`cache/library-management/blobs`) next to the compacted database.

    python -m maintenance.library_management_artwork_compaction \\
        --database /copy/library.db --blob-root /copy/blobs

That is a dry run that reports what would change. Add `--apply
--confirm-isolated-copy` to rewrite the copy (`--max-batches` bounds one run).

Rewriting rows only frees pages inside the file. To shrink `library.db`, run a manual
`VACUUM` on the stopped database; it needs free disk space roughly the size of the
database. Blobs that no finished row references any more are removed by the periodic
retention pass once the compacted database is live.
"""

from __future__ import annotations

import argparse
from contextlib import closing
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import sys
import tempfile
import time
from typing import Callable, Sequence

import msgspec

from infrastructure.library_management_blob_store import _image_metadata_json
from models.audio_metadata import DesiredAudioDocument
from models.library_management import LibraryManagementImportBundle
from models.library_management_stored_artwork import (
    embedded_artwork_choices,
    persisted_import_bundle,
    stored_embedded_artwork,
)


DEFAULT_BATCH_SIZE = 10
MAX_BATCH_SIZE = 100
_INLINE_BYTES = "instr({column}, '\"content\":\"')>0"
_BUNDLE_STATES = "('completed','resolved','rolled_back')"
_REFERENCE_PREFIX = "import-bundle:"


class ArtworkCompactionError(RuntimeError):
    """The offline copy could not be compacted without weakening recovery."""


class _Unverifiable(Exception):
    """A row that cannot be proven safe to rewrite and is left as it is."""


class ArtworkCompactionResult(msgspec.Struct, frozen=True, kw_only=True):
    dry_run: bool
    bundles_scanned: int
    bundles_compactable: int
    bundles_compacted: int
    plan_items_scanned: int
    plan_items_compactable: int
    plan_items_compacted: int
    skipped_non_terminal: int
    skipped_unverifiable: int
    blobs_written: int
    logical_bytes_reclaimable: int
    complete: bool
    foreign_key_check_passed: bool | None = None
    integrity_check_passed: bool | None = None


@dataclass(slots=True)
class _Totals:
    scanned: int = 0
    compactable: int = 0
    compacted: int = 0
    skipped_unverifiable: int = 0
    reclaimable: int = 0


@dataclass(frozen=True, slots=True)
class _Rewrite:
    """One row's verified replacement and the (payload, reference id) pairs it moves."""

    references: tuple[tuple[bytes, str], ...]
    reference_created_at: float
    reclaimable: int
    update_sql: str
    update_values: tuple[object, ...]


@dataclass(frozen=True, slots=True)
class _Kind:
    first_cursor: tuple[object, ...]
    next_keys: Callable[
        [sqlite3.Connection, tuple[object, ...], int], list[tuple[object, ...]]
    ]
    load: Callable[[sqlite3.Connection, tuple[object, ...]], sqlite3.Row]
    prepare: Callable[[sqlite3.Row], _Rewrite | None]


def _validate_isolated_paths(database: Path, blob_root: Path) -> tuple[Path, Path]:
    if database.is_symlink() or not database.is_file():
        raise ArtworkCompactionError(
            "The isolated database must be a regular, non-symlink file."
        )
    if blob_root.is_symlink() or not blob_root.is_dir():
        raise ArtworkCompactionError(
            "The isolated blob root must be a non-symlink directory."
        )
    return database.resolve(), blob_root.resolve()


def _connect(database: Path, *, dry_run: bool) -> sqlite3.Connection:
    if dry_run:
        connection = sqlite3.connect(
            f"{database.as_uri()}?mode=ro", uri=True, timeout=5
        )
    else:
        connection = sqlite3.connect(database, timeout=5)
        connection.execute("PRAGMA foreign_keys=ON")
    connection.row_factory = sqlite3.Row
    return connection


def _sha256(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def _relative_blob_path(sha256: str) -> Path:
    return Path("objects", sha256[:2], sha256[2:4], f"{sha256}.blob")


def _bundle_keys(
    connection: sqlite3.Connection, cursor: tuple[object, ...], limit: int
) -> list[tuple[object, ...]]:
    return [
        (row[0],)
        for row in connection.execute(
            "SELECT id FROM library_management_import_bundles "
            f"WHERE id>? AND state IN {_BUNDLE_STATES} "
            f"AND {_INLINE_BYTES.format(column='request_json')} ORDER BY id LIMIT ?",
            (*cursor, limit),
        )
    ]


def _load_bundle(
    connection: sqlite3.Connection, key: tuple[object, ...]
) -> sqlite3.Row:
    return connection.execute(
        "SELECT id,request_json,request_hash,created_at "
        "FROM library_management_import_bundles WHERE id=?",
        key,
    ).fetchone()


def _prepare_bundle(row: sqlite3.Row) -> _Rewrite | None:
    request_json = str(row["request_json"])
    if _sha256(request_json.encode()) != str(row["request_hash"]):
        raise _Unverifiable
    try:
        bundle = msgspec.json.decode(
            request_json.encode(), type=LibraryManagementImportBundle
        )
    except (msgspec.DecodeError, msgspec.ValidationError) as error:
        raise _Unverifiable from error
    payloads: dict[str, bytes] = {}
    for file in bundle.files:
        images = (
            file.desired_document.artwork or ()
            if file.desired_document is not None
            else ()
        )
        if file.desired_artwork is not None and images:
            raise _Unverifiable
        for image in images:
            if image.sha256 != _sha256(image.content):
                raise _Unverifiable
            payloads[image.sha256] = image.content
        for artifact in file.artifacts:
            if artifact.content is None:
                continue
            # The publisher refuses a stored payload whose fingerprint differs.
            if artifact.source_fingerprint != _sha256(artifact.content):
                raise _Unverifiable
            payloads[_sha256(artifact.content)] = artifact.content
    if not payloads:
        return None
    new_json = msgspec.json.encode(persisted_import_bundle(bundle)).decode()
    return _Rewrite(
        references=tuple(
            (content, f"{_REFERENCE_PREFIX}{row['id']}") for content in payloads.values()
        ),
        reference_created_at=float(row["created_at"]),
        reclaimable=len(request_json) - len(new_json),
        update_sql=(
            "UPDATE library_management_import_bundles SET request_json=?,"
            f"request_hash=? WHERE id=? AND request_hash=? AND state IN {_BUNDLE_STATES}"
        ),
        update_values=(
            new_json,
            _sha256(new_json.encode()),
            row["id"],
            row["request_hash"],
        ),
    )


def _plan_item_keys(
    connection: sqlite3.Connection, cursor: tuple[object, ...], limit: int
) -> list[tuple[object, ...]]:
    return [
        (row[0], row[1])
        for row in connection.execute(
            "SELECT item.job_id,item.ordinal FROM library_management_plan_items item "
            "JOIN library_management_job_snapshots snapshot "
            "ON snapshot.job_id=item.job_id "
            "WHERE snapshot.phase='complete' "
            "AND (item.job_id>? OR (item.job_id=? AND item.ordinal>?)) "
            f"AND {_INLINE_BYTES.format(column='item.desired_document_json')} "
            "ORDER BY item.job_id,item.ordinal LIMIT ?",
            (cursor[0], cursor[0], cursor[1], limit),
        )
    ]


def _load_plan_item(
    connection: sqlite3.Connection, key: tuple[object, ...]
) -> sqlite3.Row:
    return connection.execute(
        "SELECT job_id,ordinal,desired_document_json,desired_document_hash,"
        "catalog_document_json,catalog_document_hash,artwork_choices_json,created_at "
        "FROM library_management_plan_items WHERE job_id=? AND ordinal=?",
        key,
    ).fetchone()


def _prepare_plan_item(row: sqlite3.Row) -> _Rewrite | None:
    desired_json = str(row["desired_document_json"])
    if _sha256(desired_json.encode()) != str(row["desired_document_hash"]):
        raise _Unverifiable
    try:
        desired = msgspec.json.decode(desired_json.encode(), type=DesiredAudioDocument)
        choices = json.loads(str(row["artwork_choices_json"]))
    except (msgspec.DecodeError, msgspec.ValidationError, ValueError) as error:
        raise _Unverifiable from error
    if not desired.artwork:
        return None
    # An item that already names embedded artwork keeps those choices as they are.
    if not isinstance(choices, list) or any(
        isinstance(choice, dict) and choice.get("output_kind") == "embedded"
        for choice in choices
    ):
        raise _Unverifiable
    if any(image.sha256 != _sha256(image.content) for image in desired.artwork):
        raise _Unverifiable
    catalog_json = row["catalog_document_json"]
    catalog_hash = row["catalog_document_hash"]
    if catalog_json is not None and catalog_json == desired_json:
        if catalog_hash != row["desired_document_hash"]:
            raise _Unverifiable
        catalog_json = catalog_hash = None
    elif catalog_json is not None:
        # A different catalog document keeps its own hash, and may not carry bytes.
        try:
            catalog = msgspec.json.decode(
                str(catalog_json).encode(), type=DesiredAudioDocument
            )
        except (msgspec.DecodeError, msgspec.ValidationError) as error:
            raise _Unverifiable from error
        if catalog.artwork or _sha256(str(catalog_json).encode()) != catalog_hash:
            raise _Unverifiable
    new_desired_json = msgspec.json.encode(
        msgspec.structs.replace(desired, artwork=None)
    ).decode()
    new_choices_json = json.dumps(
        [*embedded_artwork_choices(stored_embedded_artwork(desired.artwork)), *choices],
        separators=(",", ":"),
        sort_keys=True,
    )
    job_id, ordinal = row["job_id"], row["ordinal"]
    old_size = (
        len(desired_json)
        + len(str(row["catalog_document_json"] or ""))
        + len(str(row["artwork_choices_json"]))
    )
    new_size = len(new_desired_json) + len(str(catalog_json or "")) + len(
        new_choices_json
    )
    return _Rewrite(
        references=tuple(
            (image.content, f"{job_id}:{ordinal}:{index}")
            for index, image in enumerate(desired.artwork)
        ),
        reference_created_at=float(row["created_at"]),
        reclaimable=old_size - new_size,
        update_sql=(
            "UPDATE library_management_plan_items SET desired_document_json=?,"
            "desired_document_hash=?,catalog_document_json=?,catalog_document_hash=?,"
            "artwork_choices_json=? WHERE job_id=? AND ordinal=? "
            "AND desired_document_hash=? AND artwork_choices_json=?"
        ),
        update_values=(
            new_desired_json,
            _sha256(new_desired_json.encode()),
            catalog_json,
            catalog_hash,
            new_choices_json,
            job_id,
            ordinal,
            row["desired_document_hash"],
            row["artwork_choices_json"],
        ),
    )


_BUNDLES = _Kind(("",), _bundle_keys, _load_bundle, _prepare_bundle)
_PLAN_ITEMS = _Kind(("", -1), _plan_item_keys, _load_plan_item, _prepare_plan_item)


def _reject_symlinks(blob_root: Path, path: Path) -> None:
    current = blob_root
    for part in path.relative_to(blob_root).parts:
        current /= part
        try:
            if stat.S_ISLNK(current.lstat().st_mode):
                raise ArtworkCompactionError("A blob path contains a symbolic link.")
        except FileNotFoundError:
            return


def _verified_blob_exists(path: Path, content: bytes) -> bool:
    try:
        return path.read_bytes() == content
    except FileNotFoundError:
        return False


def _write_blob(blob_root: Path, content: bytes) -> bool:
    """Publish one content-addressed blob file; False when it was already there."""

    path = blob_root / _relative_blob_path(_sha256(content))
    _reject_symlinks(blob_root, path)
    if path.exists():
        if not _verified_blob_exists(path, content):
            raise ArtworkCompactionError(
                "An existing blob does not match its content hash."
            )
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary_root = blob_root / ".tmp"
    temporary_root.mkdir(exist_ok=True)
    descriptor, raw_path = tempfile.mkstemp(
        prefix="management-", suffix=".tmp", dir=temporary_root
    )
    temporary = Path(raw_path)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        try:
            os.link(temporary, path)
        except FileExistsError:
            if not _verified_blob_exists(path, content):
                raise ArtworkCompactionError(
                    "An existing blob does not match its content hash."
                ) from None
            return False
    finally:
        temporary.unlink(missing_ok=True)
    directory = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
    return True


def _write_payloads(blob_root: Path, rewrites: Sequence[_Rewrite]) -> int:
    written = 0
    for rewrite in rewrites:
        for content, _reference_id in rewrite.references:
            written += _write_blob(blob_root, content)
            if not _verified_blob_exists(
                blob_root / _relative_blob_path(_sha256(content)), content
            ):
                raise ArtworkCompactionError("A written blob failed read-back.")
    return written


def _register_blob(connection: sqlite3.Connection, content: bytes, now: float) -> None:
    sha256 = _sha256(content)
    relative = _relative_blob_path(sha256).as_posix()
    metadata = _image_metadata_json(content)
    connection.execute(
        "INSERT OR IGNORE INTO library_management_blobs "
        "(sha256,kind,byte_length,relative_path,media_metadata_json,created_at,"
        "row_revision) VALUES (?,'image',?,?,?,?,1)",
        (sha256, len(content), relative, metadata, now),
    )
    row = connection.execute(
        "SELECT kind,byte_length,relative_path,media_metadata_json "
        "FROM library_management_blobs WHERE sha256=?",
        (sha256,),
    ).fetchone()
    if int(row["byte_length"]) != len(content) or str(row["relative_path"]) != relative:
        raise ArtworkCompactionError(
            "A content hash is already registered with different metadata."
        )
    if str(row["kind"]) != "image" or (
        str(row["media_metadata_json"]) == "{}" and metadata != "{}"
    ):
        connection.execute(
            "UPDATE library_management_blobs SET kind='image',media_metadata_json=?,"
            "row_revision=row_revision+1 WHERE sha256=?",
            (metadata, sha256),
        )


def _apply_rewrites(
    connection: sqlite3.Connection, rewrites: Sequence[_Rewrite], now: float
) -> None:
    connection.execute("BEGIN IMMEDIATE")
    try:
        for rewrite in rewrites:
            for content, reference_id in rewrite.references:
                _register_blob(connection, content, now)
                connection.execute(
                    "INSERT OR IGNORE INTO library_management_blob_references "
                    "(blob_sha256,reference_kind,reference_id,created_at) "
                    "VALUES (?,'artwork',?,?)",
                    (_sha256(content), reference_id, rewrite.reference_created_at),
                )
            if connection.execute(rewrite.update_sql, rewrite.update_values).rowcount != 1:
                raise ArtworkCompactionError(
                    "A row changed after verification; no batch was compacted."
                )
        connection.commit()
    except Exception:
        connection.rollback()
        raise


def _compact_kind(
    connection: sqlite3.Connection,
    blob_root: Path,
    kind: _Kind,
    totals: _Totals,
    *,
    apply: bool,
    batch_size: int,
    batches_left: int | None,
) -> tuple[bool, int, int]:
    """Walk one table in key order; returns (finished, batches used, blobs written)."""

    cursor = kind.first_cursor
    batches = blobs_written = 0
    while True:
        keys = kind.next_keys(connection, cursor, batch_size)
        if not keys:
            return True, batches, blobs_written
        if batches_left is not None and batches >= batches_left:
            return False, batches, blobs_written
        cursor = keys[-1]
        rewrites: list[_Rewrite] = []
        for key in keys:
            totals.scanned += 1
            try:
                rewrite = kind.prepare(kind.load(connection, key))
            except _Unverifiable:
                totals.skipped_unverifiable += 1
                continue
            if rewrite is not None:
                totals.compactable += 1
                totals.reclaimable += rewrite.reclaimable
                rewrites.append(rewrite)
        if apply and rewrites:
            blobs_written += _write_payloads(blob_root, rewrites)
            _apply_rewrites(connection, rewrites, time.time())
            totals.compacted += len(rewrites)
        batches += 1


def _count_non_terminal(connection: sqlite3.Connection) -> int:
    bundles = connection.execute(
        "SELECT COUNT(*) FROM library_management_import_bundles "
        f"WHERE state NOT IN {_BUNDLE_STATES} "
        f"AND {_INLINE_BYTES.format(column='request_json')}"
    ).fetchone()[0]
    items = connection.execute(
        "SELECT COUNT(*) FROM library_management_plan_items item "
        "JOIN library_management_job_snapshots snapshot ON snapshot.job_id=item.job_id "
        "WHERE snapshot.phase<>'complete' "
        f"AND {_INLINE_BYTES.format(column='item.desired_document_json')}"
    ).fetchone()[0]
    return int(bundles) + int(items)


def compact_artwork_copy(
    database: Path,
    blob_root: Path,
    *,
    apply: bool = False,
    batch_size: int = DEFAULT_BATCH_SIZE,
    max_batches: int | None = None,
) -> ArtworkCompactionResult:
    """Verify and optionally rewrite finished rows on an isolated copy."""

    if batch_size < 1 or batch_size > MAX_BATCH_SIZE:
        raise ArtworkCompactionError(
            f"Batch size must be between 1 and {MAX_BATCH_SIZE}."
        )
    if max_batches is not None and max_batches < 1:
        raise ArtworkCompactionError("Max batches must be at least 1.")
    database, blob_root = _validate_isolated_paths(database, blob_root)
    bundles, items = _Totals(), _Totals()
    with closing(_connect(database, dry_run=not apply)) as connection:
        finished, used, blobs_written = _compact_kind(
            connection,
            blob_root,
            _BUNDLES,
            bundles,
            apply=apply,
            batch_size=batch_size,
            batches_left=max_batches,
        )
        if finished:
            finished, more_used, more_written = _compact_kind(
                connection,
                blob_root,
                _PLAN_ITEMS,
                items,
                apply=apply,
                batch_size=batch_size,
                batches_left=None if max_batches is None else max_batches - used,
            )
            blobs_written += more_written
        foreign_keys_passed: bool | None = None
        integrity_passed: bool | None = None
        if apply and finished:
            foreign_keys_passed = (
                connection.execute("PRAGMA foreign_key_check").fetchone() is None
            )
            integrity = connection.execute("PRAGMA integrity_check").fetchone()
            integrity_passed = integrity is not None and str(integrity[0]) == "ok"
            if not foreign_keys_passed or not integrity_passed:
                raise ArtworkCompactionError(
                    "The compacted copy failed SQLite integrity validation."
                )
        return ArtworkCompactionResult(
            dry_run=not apply,
            bundles_scanned=bundles.scanned,
            bundles_compactable=bundles.compactable,
            bundles_compacted=bundles.compacted,
            plan_items_scanned=items.scanned,
            plan_items_compactable=items.compactable,
            plan_items_compacted=items.compacted,
            skipped_non_terminal=_count_non_terminal(connection),
            skipped_unverifiable=(
                bundles.skipped_unverifiable + items.skipped_unverifiable
            ),
            blobs_written=blobs_written,
            logical_bytes_reclaimable=bundles.reclaimable + items.reclaimable,
            complete=finished,
            foreign_key_check_passed=foreign_keys_passed,
            integrity_check_passed=integrity_passed,
        )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Move artwork bytes out of finished Library Management import rows on an "
            "isolated database copy"
        )
    )
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--blob-root", type=Path, required=True)
    parser.add_argument("--batch-size", type=int, default=DEFAULT_BATCH_SIZE)
    parser.add_argument("--max-batches", type=int)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--confirm-isolated-copy", action="store_true")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    if args.apply and not args.confirm_isolated_copy:
        parser.error("--apply requires --confirm-isolated-copy")
    try:
        result = compact_artwork_copy(
            args.database,
            args.blob_root,
            apply=args.apply,
            batch_size=args.batch_size,
            max_batches=args.max_batches,
        )
    except (ArtworkCompactionError, sqlite3.Error) as error:
        print(f"Artwork compaction refused: {error}", file=sys.stderr)
        return 1
    print(msgspec.json.encode(result).decode())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
