from typing import Literal

from infrastructure.msgspec_fastapi import AppStruct

GenrePrefLevel = Literal["normal", "reduce", "mute"]


class GenrePrefItem(AppStruct):
    family: str
    label: str
    artist_count: int = 0
    level: GenrePrefLevel = "normal"


class GenrePrefsResponse(AppStruct):
    genres: list[GenrePrefItem] = []


class GenrePrefUpdateItem(AppStruct):
    family: str
    level: GenrePrefLevel


class GenrePrefsUpdate(AppStruct):
    genres: list[GenrePrefUpdateItem] = []
