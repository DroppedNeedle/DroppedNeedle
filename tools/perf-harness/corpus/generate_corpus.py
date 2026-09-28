#!/usr/bin/env python3
"""Deterministic synthesized-library generator (needs mutagen, no ffmpeg).

Builds an N-track music corpus by byte-copying committed fixture audio and
retagging each copy with deterministic unique tags. No ffmpeg needed: the
audio payloads are the fixtures themselves (~0.3 s silence, 1-55 KB), so
scan rates measured on this corpus are discovery+tag+index bound — the same
caveat as the Stage-0 baseline.

Determinism: --seed drives template assignment; file mtimes are fixed
(1700000000 + index); tags derive from indices; the manifest is index-sorted
regardless of --jobs.

Layout: <out>/artist-<ai>/album-<ai>-<aj>/<track>-<slug>.<ext>
Default shape for 100k: 500 artists x 20 albums x 10 tracks.

Only extensions with simple reliable mutagen round-trips are used as
templates (flac/mp3/m4a/ogg/opus). .wma is excluded (v3 cuts WMA);
.aac/.wav fixtures are excluded (APE/RIFF tag paths are fiddly and add no
coverage for the budget question).

Usage:
  backend/.venv/bin/python tools/perf-harness/corpus/generate_corpus.py \
      --tracks 100000 --seed 1 --out /var/tmp/v3_s1_corpus_100k [--jobs 4]

Outputs: audio files + manifest.jsonl + manifest.sha256 + corpus.json.
Rerunning with the same flags onto an empty dir reproduces identical bytes.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import random
import shutil
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import common  # noqa: E402

try:
    import mutagen
    from mutagen.flac import FLAC
    from mutagen.id3 import ID3, TALB, TCON, TDRC, TIT2, TPE1, TPE2, TPOS, TRCK
    from mutagen.mp4 import MP4
    from mutagen.oggopus import OggOpus
    from mutagen.oggvorbis import OggVorbis
except ImportError:
    sys.exit("mutagen is required: run with backend/.venv/bin/python")

MTIME_BASE = 1700000000  # fixed epoch; file mtime = base + index
TEMPLATE_EXTS = (".flac", ".mp3", ".m4a", ".ogg", ".opus")


def list_templates() -> list[Path]:
    """Committed fixture audio usable as copy payloads (sorted, deterministic)."""
    templates = [
        path
        for path in sorted(common.FIXTURE_DIR.iterdir())
        if path.suffix.lower() in TEMPLATE_EXTS and path.is_file()
    ]
    if not templates:
        raise SystemExit(f"no templates found in {common.FIXTURE_DIR}")
    return templates


def plan_shape(total: int, albums_per_artist: int, tracks_per_album: int) -> tuple[int, int, int]:
    """Return (artists, albums_per_artist, tracks_per_album) covering >= total."""
    per_artist = albums_per_artist * tracks_per_album
    artists = (total + per_artist - 1) // per_artist
    return artists, albums_per_artist, tracks_per_album


def tags_for(ai: int, aj: int, t: int, track_no: int) -> dict:
    """Deterministic unique tags. 'Perf' is the search-hit term for benches."""
    return {
        "title": f"Perf Track {track_no:06d}",
        "artist": f"Perf Artist {ai:04d}",
        "album": f"Perf Album {ai:04d}-{aj:03d}",
        "album_artist": f"Perf Artist {ai:04d}",
        "track_number": str(t),
        "disc_number": "1",
        "date": f"{2000 + (track_no % 25)}",
        "genre": "PerfGenre",
    }


def retag(path: Path, tags: dict) -> None:
    """Replace ALL tags with the synthesized set (strips fixture MBIDs)."""
    ext = path.suffix.lower()
    if ext == ".flac":
        audio = FLAC(path)
        audio.delete()
        audio["TITLE"] = [tags["title"]]
        audio["ARTIST"] = [tags["artist"]]
        audio["ALBUM"] = [tags["album"]]
        audio["ALBUMARTIST"] = [tags["album_artist"]]
        audio["TRACKNUMBER"] = [tags["track_number"]]
        audio["DISCNUMBER"] = [tags["disc_number"]]
        audio["DATE"] = [tags["date"]]
        audio["GENRE"] = [tags["genre"]]
        audio.save()
    elif ext in (".ogg", ".opus"):
        cls = OggOpus if ext == ".opus" else OggVorbis
        audio = cls(path)
        audio.delete()
        audio["TITLE"] = [tags["title"]]
        audio["ARTIST"] = [tags["artist"]]
        audio["ALBUM"] = [tags["album"]]
        audio["ALBUMARTIST"] = [tags["album_artist"]]
        audio["TRACKNUMBER"] = [tags["track_number"]]
        audio["DISCNUMBER"] = [tags["disc_number"]]
        audio["DATE"] = [tags["date"]]
        audio["GENRE"] = [tags["genre"]]
        audio.save()
    elif ext == ".mp3":
        audio = ID3(path)
        audio.delete()
        fresh = ID3()
        fresh.add(TIT2(encoding=3, text=[tags["title"]]))
        fresh.add(TPE1(encoding=3, text=[tags["artist"]]))
        fresh.add(TALB(encoding=3, text=[tags["album"]]))
        fresh.add(TPE2(encoding=3, text=[tags["album_artist"]]))
        fresh.add(TRCK(encoding=3, text=[tags["track_number"]]))
        fresh.add(TPOS(encoding=3, text=[tags["disc_number"]]))
        fresh.add(TDRC(encoding=3, text=[tags["date"]]))
        fresh.add(TCON(encoding=3, text=[tags["genre"]]))
        fresh.save(path)
    elif ext == ".m4a":
        audio = MP4(path)
        audio.delete()
        audio["\xa9nam"] = [tags["title"]]
        audio["\xa9ART"] = [tags["artist"]]
        audio["\xa9alb"] = [tags["album"]]
        audio["aART"] = [tags["album_artist"]]
        audio["trkn"] = [(int(tags["track_number"]), 0)]
        audio["disk"] = [(int(tags["disc_number"]), 0)]
        audio["\xa9day"] = [tags["date"]]
        audio["\xa9gen"] = [tags["genre"]]
        audio.save()
    else:  # pragma: no cover — guarded by TEMPLATE_EXTS
        raise ValueError(f"unsupported template ext {ext}")


def build_one(
    index: int, template: Path, dest: Path, tags: dict
) -> dict:
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(template, dest)
    retag(dest, tags)
    mtime = MTIME_BASE + index
    os.utime(dest, (mtime, mtime))
    return {
        "index": index,
        "path": str(dest),
        "sha256": common.sha256_file(dest),
        "size": dest.stat().st_size,
        "template": template.name,
        **tags,
    }


def verify_sample(out: Path, records: list[dict], stride: int = 503) -> dict:
    """Re-read every stride-th file; confirm the deterministic title survived."""
    checked, mismatched = 0, 0
    for record in records[::stride]:
        checked += 1
        path = Path(record["path"])
        ext = path.suffix.lower()
        try:
            if ext == ".flac":
                title = FLAC(path)["TITLE"][0]
            elif ext == ".ogg":
                title = OggVorbis(path)["TITLE"][0]
            elif ext == ".opus":
                title = OggOpus(path)["TITLE"][0]
            elif ext == ".mp3":
                title = ID3(path)["TIT2"].text[0]
            elif ext == ".m4a":
                title = MP4(path)["\xa9nam"][0]
            else:
                title = None
        except Exception:
            title = None
        if title != record["title"]:
            mismatched += 1
    _ = out
    return {"verify_checked": checked, "verify_mismatched": mismatched}


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate a deterministic track corpus.")
    parser.add_argument("--tracks", type=int, required=True, help="Total tracks to generate")
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--out", required=True, help="Output dir (must be empty or absent)")
    parser.add_argument("--albums-per-artist", type=int, default=20)
    parser.add_argument("--tracks-per-album", type=int, default=10)
    parser.add_argument("--jobs", type=int, default=4, help="Retag worker threads")
    parser.add_argument("--no-verify", action="store_true")
    args = parser.parse_args()

    if args.tracks <= 0:
        raise SystemExit("--tracks must be positive")
    common.assert_safe_target(paths=[args.out])
    out = Path(args.out)
    if out.exists() and any(out.iterdir()):
        raise SystemExit(f"refusing non-empty output dir {out}")

    templates = list_templates()
    rng = random.Random(args.seed)
    artists, albums_per_artist, tracks_per_album = plan_shape(
        args.tracks, args.albums_per_artist, args.tracks_per_album
    )

    # Deterministic work list: index -> (template, dest, tags).
    jobs: list[tuple[int, Path, Path, dict]] = []
    index = 0
    for ai in range(artists):
        for aj in range(albums_per_artist):
            for t in range(1, tracks_per_album + 1):
                if index >= args.tracks:
                    break
                template = templates[rng.randrange(len(templates))]
                tags = tags_for(ai, aj, t, index)
                dest = (
                    out
                    / f"artist-{ai:04d}"
                    / f"album-{ai:04d}-{aj:03d}"
                    / f"{t:02d}-perf-track-{index:06d}{template.suffix.lower()}"
                )
                jobs.append((index, template, dest, tags))
                index += 1
            if index >= args.tracks:
                break
        if index >= args.tracks:
            break

    started = time.perf_counter()
    records: list[dict] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = [pool.submit(build_one, i, tmpl, dest, tags) for i, tmpl, dest, tags in jobs]
        for count, future in enumerate(concurrent.futures.as_completed(futures), 1):
            records.append(future.result())
            if count % 5000 == 0:
                elapsed = time.perf_counter() - started
                print(f"  ... {count}/{len(jobs)} in {elapsed:.1f}s", file=sys.stderr)
    records.sort(key=lambda r: r["index"])
    elapsed = time.perf_counter() - started

    manifest = out / "manifest.jsonl"
    with open(manifest, "w") as handle:
        for record in records:
            record["path"] = str(Path(record["path"]).relative_to(out))
            handle.write(json.dumps(record) + "\n")
    manifest_sha = common.sha256_file(manifest)
    (out / "manifest.sha256").write_text(f"{manifest_sha}  manifest.jsonl\n")
    total_bytes = sum(r["size"] for r in records)

    summary = {
        "meta": common.run_metadata({"seed": args.seed, "mutagen": mutagen.version_string}),
        "tracks": len(records),
        "artists": artists,
        "albums_per_artist": albums_per_artist,
        "tracks_per_album": tracks_per_album,
        "templates": [t.name for t in templates],
        "total_bytes": total_bytes,
        "elapsed_s": round(elapsed, 1),
        "files_per_sec": round(len(records) / elapsed, 1) if elapsed > 0 else None,
        "manifest_sha256": manifest_sha,
    }
    if not args.no_verify:
        # Re-resolve absolute paths for verification.
        for record in records:
            record["path"] = str(out / record["path"])
        summary.update(verify_sample(out, records))
        if summary["verify_mismatched"]:
            raise SystemExit(f"verify FAILED: {summary['verify_mismatched']} mismatches")
    common.write_json(out / "corpus.json", summary)
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
