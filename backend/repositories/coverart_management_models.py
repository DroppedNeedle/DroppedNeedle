"""Live-verified Cover Art Archive management wire structs."""

import msgspec


class CaaManagementThumbnails(msgspec.Struct):
    size_1200: str | None = msgspec.field(name="1200", default=None)
    size_500: str | None = msgspec.field(name="500", default=None)
    size_250: str | None = msgspec.field(name="250", default=None)


class CaaManagementImage(msgspec.Struct):
    approved: bool = False
    back: bool = False
    comment: str = ""
    front: bool = False
    id: int | str = 0
    image: str = ""
    thumbnails: CaaManagementThumbnails = msgspec.field(
        default_factory=CaaManagementThumbnails
    )
    types: list[str] = msgspec.field(default_factory=list)


class CaaManagementResponse(msgspec.Struct):
    images: list[CaaManagementImage] = msgspec.field(default_factory=list)
