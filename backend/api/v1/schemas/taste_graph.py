"""Taste Graph — recommendations derived ONLY from this user's own signals
(library, follows, play history) expanded through canonical MusicBrainz
metadata. No external charts, no global popularity, no other users' data."""

from typing import Literal

from infrastructure.msgspec_fastapi import AppStruct


class TasteGraphReason(AppStruct):
    type: Literal["collaborator", "member", "label", "scene"]
    label: str
    via_mbid: str | None = None
    via_name: str | None = None


class TasteGraphSeed(AppStruct):
    artist_mbid: str
    name: str
    weight: float


class TasteGraphItem(AppStruct):
    kind: Literal["artist", "album"]
    mbid: str
    name: str
    score: float
    reasons: list[TasteGraphReason] = []
    artist_mbid: str | None = None
    artist_name: str | None = None
    in_library: bool = False


class TasteGraphResponse(AppStruct):
    cold_start: bool = False
    generated_at: str = ""
    seeds: list[TasteGraphSeed] = []
    items: list[TasteGraphItem] = []
