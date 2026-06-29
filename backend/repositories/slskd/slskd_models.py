"""msgspec structs mirroring slskd 0.25.1 JSON shapes.

Shapes verified against a live slskd 0.25.1.0 instance (see ``API_NOTES.md``).
PRIVATE to the slskd package; never cross into ``services/native`` (that
boundary uses the protocol types). snake_case fields are mapped to slskd's
camelCase wire keys via ``rename``; unknown fields are ignored (default).
"""

import msgspec


class SlskdFile(msgspec.Struct, rename="camel", kw_only=True):
    """A single file in a peer's search response.

    ``bitRate`` is ABSENT for lossless files (left None, do not coerce to 0);
    ``extension`` can be empty even for a real file, so parse the extension
    from ``filename`` instead (API_NOTES.md C6).
    """

    filename: str
    size: int = 0
    extension: str = ""
    bit_depth: int | None = None
    sample_rate: int | None = None
    length: float | None = None  # track duration in seconds
    bit_rate: int | None = None
    code: int | None = None
    is_locked: bool = False


class SlskdUserSearchResponse(msgspec.Struct, rename="camel", kw_only=True):
    username: str
    has_free_upload_slot: bool = False
    upload_speed: int = 0
    queue_length: int = 0
    file_count: int = 0
    locked_file_count: int = 0
    files: list[SlskdFile] = []
    locked_files: list[SlskdFile] = []
    token: int | None = None


class SlskdSearchResponse(msgspec.Struct, rename="camel", kw_only=True):
    """``state`` is a comma-joined flags string (e.g. ``"Completed, Succeeded"``)."""

    id: str
    state: str = ""
    is_complete: bool = False
    search_text: str = ""
    file_count: int = 0
    response_count: int = 0
    locked_file_count: int = 0
    token: int | None = None


class SlskdEnqueueResponse(msgspec.Struct, rename="pascal", kw_only=True):
    """201 body from enqueue. Keys are PascalCase (``Enqueued`` / ``Failed``);
    no batch GUID, each file becomes its own transfer (C2)."""

    enqueued: list = []
    failed: list = []


class SlskdTransfer(msgspec.Struct, rename="camel", kw_only=True):
    id: str
    username: str = ""
    filename: str = ""
    size: int = 0
    bytes_transferred: int = 0
    bytes_remaining: int = 0
    percent_complete: float = 0.0
    average_speed: float = 0.0
    state: str = ""
    direction: str = ""
    place_in_queue: int | None = None
    exception: str | None = None


class SlskdDirectories(msgspec.Struct, kw_only=True):
    """``directories`` block of GET /api/v0/options - where slskd saves files (verified
    keys: ``downloads``, ``incomplete``; both are slskd's in-container paths)."""

    downloads: str = ""
    incomplete: str = ""


class SlskdOptions(msgspec.Struct, kw_only=True):
    """Subset of GET /api/v0/options we use: just the directories block, so DroppedNeedle
    can tell the user the exact path slskd downloads to. Unknown fields ignored."""

    directories: SlskdDirectories = msgspec.field(default_factory=SlskdDirectories)
