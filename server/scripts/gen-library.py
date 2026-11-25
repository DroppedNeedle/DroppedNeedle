"""Write a generated music library for the small-machine perf run.

Every file carries the audio of one committed FLAC fixture with a fresh
Vorbis comment block, in the shape the scan throughput test uses: eight
tracks to an album, four albums to an artist, and MusicBrainz ids that
differ per artist, release group, release and track. The files land at
<out>/ArtNNNNNN/AlbumNNNNNN/NN.flac.

Standard library only, so it runs on a bare CI runner:

    python3 -I server/scripts/gen-library.py <out> --tracks 100000
"""

import argparse
import os
import struct
import sys
import uuid

TRACKS_PER_ALBUM = 8
ALBUMS_PER_ARTIST = 4
VORBIS_COMMENT = 4
PADDING = 1
VENDOR = b"DroppedNeedle gen-library"
# Fixed so two runs write the same library.
NAMESPACE = uuid.UUID("6f1d2b1e-5a43-4c84-9a3e-2b8f0d6a7c10")

DEFAULT_FIXTURE = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..",
    "tests",
    "fixtures",
    "library",
    "flac_full_01.flac",
)


def mbid(kind, number):
    return str(uuid.uuid5(NAMESPACE, f"{kind}:{number}"))


def split_flac(data):
    """Return the metadata blocks other than the comment and padding, and
    the audio frames after them."""
    if data[:4] != b"fLaC":
        raise ValueError("the fixture is not a FLAC file")
    at = 4
    blocks = []
    while True:
        if at + 4 > len(data):
            raise ValueError("the fixture's metadata runs past its end")
        header = data[at]
        kind = header & 0x7F
        length = int.from_bytes(data[at + 1 : at + 4], "big")
        body = data[at + 4 : at + 4 + length]
        if len(body) != length:
            raise ValueError("the fixture's metadata runs past its end")
        if kind not in (VORBIS_COMMENT, PADDING):
            blocks.append((kind, body))
        at += 4 + length
        if header & 0x80:
            break
    if not blocks or blocks[0][0] != 0:
        raise ValueError("the fixture has no STREAMINFO block first")
    return blocks, data[at:]


def comment_block(fields):
    out = bytearray(struct.pack("<I", len(VENDOR)) + VENDOR)
    out += struct.pack("<I", len(fields))
    for name, value in fields:
        entry = f"{name}={value}".encode("utf-8")
        out += struct.pack("<I", len(entry)) + entry
    return bytes(out)


def flac_bytes(blocks, audio, fields):
    out = bytearray(b"fLaC")
    every = blocks + [(VORBIS_COMMENT, comment_block(fields))]
    for index, (kind, body) in enumerate(every):
        last = 0x80 if index == len(every) - 1 else 0
        out.append(last | kind)
        out += len(body).to_bytes(3, "big")
        out += body
    out += audio
    return bytes(out)


def track_fields(index):
    album = index // TRACKS_PER_ALBUM
    artist = album // ALBUMS_PER_ARTIST
    track = index % TRACKS_PER_ALBUM + 1
    artist_name = f"Art{artist:06}"
    artist_id = mbid("artist", artist)
    return (
        artist_name,
        f"Album{album:06}",
        track,
        [
            ("TITLE", f"Song {track}"),
            ("ARTIST", artist_name),
            ("ARTISTS", artist_name),
            ("ALBUM", f"Album{album:06}"),
            ("ALBUMARTIST", artist_name),
            ("ALBUMARTISTS", artist_name),
            ("TRACKNUMBER", str(track)),
            ("TRACKTOTAL", str(TRACKS_PER_ALBUM)),
            ("DISCNUMBER", "1"),
            ("DATE", str(1960 + album % 60)),
            ("GENRE", "Alternative Rock"),
            ("MUSICBRAINZ_ARTISTID", artist_id),
            ("MUSICBRAINZ_ALBUMARTISTID", artist_id),
            ("MUSICBRAINZ_RELEASEGROUPID", mbid("release-group", album)),
            ("MUSICBRAINZ_ALBUMID", mbid("release", album)),
            ("MUSICBRAINZ_RELEASETRACKID", mbid("track", index)),
            ("MUSICBRAINZ_TRACKID", mbid("recording", index)),
        ],
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", help="folder to write the library into; must be empty or absent")
    parser.add_argument("--tracks", type=int, default=100_000, help="number of files (default 100000)")
    parser.add_argument("--fixture", default=DEFAULT_FIXTURE, help="FLAC file whose audio every track carries")
    args = parser.parse_args()

    if args.tracks < 1:
        parser.error("--tracks must be at least 1")
    if os.path.isdir(args.out) and os.listdir(args.out):
        parser.error(f"{args.out} is not empty")

    with open(args.fixture, "rb") as handle:
        blocks, audio = split_flac(handle.read())

    written = 0
    for index in range(args.tracks):
        artist, album, track, fields = track_fields(index)
        folder = os.path.join(args.out, artist, album)
        if track == 1 or index == 0:
            os.makedirs(folder, exist_ok=True)
        data = flac_bytes(blocks, audio, fields)
        with open(os.path.join(folder, f"{track:02}.flac"), "wb") as handle:
            handle.write(data)
        written += len(data)

    albums = (args.tracks + TRACKS_PER_ALBUM - 1) // TRACKS_PER_ALBUM
    artists = (albums + ALBUMS_PER_ARTIST - 1) // ALBUMS_PER_ARTIST
    print(f"{args.tracks} tracks, {albums} albums, {artists} artists, {written} bytes of files in {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
