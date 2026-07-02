"""Decision vocabulary for the acquisition spec pipeline (ArrRebuild step 1).

A spec is a pure function ``(candidate, target, context, policy) -> Decision``.
``Decision`` is ``Accept | Reject``; ``Reject`` carries a typed ``code`` and a
``disposition`` that later steps map DIRECTLY onto failover/blocklist/retry
behaviour (design principle #3 - structured outcomes, not Lidarr's loose strings).

Nothing here does I/O or imports the API ``DownloadPolicySettings`` schema: this is
the vocabulary the pure specs speak, decoupled so they stay unit-testable with zero
mocks. The single I/O step (``DecisionContext`` / ``build_context``) lives in
``context.py``.
"""

import enum

import msgspec


class RejectCode(enum.Enum):
    """Why a candidate was rejected. Drives the ``dropped_*`` telemetry today and
    failover/blocklist routing in later steps. Grows per migration step."""

    BLOCKLISTED = "blocklisted"
    PASSWORD_PROTECTED = "password_protected"
    WRONG_EDITION = "wrong_edition"
    WRONG_ALBUM = "wrong_album"
    SAMPLE = "sample"
    IGNORED_TERM = "ignored_term"
    REQUIRED_TERM_MISSING = "required_term_missing"
    QUALITY_REJECTED = "quality_rejected"
    NOT_AN_UPGRADE = "not_an_upgrade"
    MAX_SIZE_EXCEEDED = "max_size_exceeded"
    RETENTION_EXCEEDED = "retention_exceeded"
    TOO_YOUNG = "too_young"
    INSUFFICIENT_SPACE = "insufficient_space"


class Disposition(enum.Enum):
    """What a rejection means for retry/failover (principle #3).

    - ``permanent``: this exact candidate can never become acceptable (wrong album,
      out-of-range quality, blocklisted) - skip it and fail over to the next.
    - ``temporary``: could succeed later (Usenet propagation, a transient stall) -
      eligible for a future retry, never permanently blocklisted.
    - ``local_fault``: OUR side failed (disk full, write rejected) - never blame or
      blocklist the source.
    """

    PERMANENT = "permanent"
    TEMPORARY = "temporary"
    LOCAL_FAULT = "local_fault"


class Accept(msgspec.Struct, frozen=True):
    """The candidate passed this spec (or the whole pipeline)."""


class Reject(msgspec.Struct, frozen=True):
    """The candidate failed a spec. ``code`` is machine-routable; ``disposition``
    decides retry/blocklist; ``detail`` is human-readable context for logs."""

    code: RejectCode
    detail: str = ""
    disposition: Disposition = Disposition.PERMANENT


Decision = Accept | Reject


class Candidate(msgspec.Struct, frozen=True, kw_only=True):
    """The normalised grab-time decision unit - the source-agnostic projection of a
    Soulseek folder group or a Usenet release that the pure specs read.

    Until ``SourceStrategy`` lands (step 4) each scorer builds this from its own raw
    shape; the specs never see ``DownloadSearchResult`` / ``UsenetRelease`` directly.

    - ``source``   - ``"soulseek"`` | ``"usenet"`` (the quarantine namespace).
    - ``identity`` - the per-source quarantine identity string (see
      ``models.download_identity``); ``""`` when the unit carries none (a Soulseek
      folder, whose quarantine is applied per-file before grouping).
    - ``match_text`` - the folder/title text the name-based specs read.
    - ``tier`` - the quality tier (``""`` / ``"unknown"`` when undetermined).
    - ``size_bytes`` - the unit's total byte size (folder audio sum / release size).
    - ``usenet_date`` - release post time (unix); ``None`` for Soulseek (no age concept).
    - ``password`` - Newznab password flag (0/absent = none); always 0 for Soulseek.
    """

    source: str
    identity: str = ""
    match_text: str = ""
    tier: str = ""
    size_bytes: int = 0
    usenet_date: float | None = None
    password: int = 0


class SpecPolicy(msgspec.Struct, frozen=True, kw_only=True):
    """The config a spec reads - decoupled from the API ``DownloadPolicySettings``
    schema so specs stay pure and independently testable. Grows per migration step.

    ``ignored_terms`` / ``required_terms`` are tuples (frozen-struct-safe). Each term
    is a plain case-insensitive substring, or a ``/regex/i`` when wrapped in slashes.
    Size/age limits use ``0`` to mean "no limit" (safe-off default), so an
    unconfigured install gets the pre-rebuild behaviour."""

    quality_min: str = "mp3_320"
    quality_max: str = "lossless"
    max_size_mb: int = 0
    ignored_terms: tuple[str, ...] = ()
    required_terms: tuple[str, ...] = ()
    # Usenet: drop a release older than this many days (past provider retention -> can't
    # be fetched). 0 = no limit.
    usenet_retention_days: int = 0
    # Usenet grab-time propagation gate: skip a release younger than this many minutes so
    # it can propagate first. 0 = off (Lidarr's MinimumAge default). NOT the same knob as
    # the post-fail leniency ``usenet_min_release_age_minutes`` on DownloadPolicySettings.
    usenet_min_age_minutes: int = 0
