"""Wanted-watcher domain model (Wanted watcher plan §5.1).

A ``WantedWatch`` is one album-level availability watch, created automatically
when a request fails for availability reasons (kind='missing') or completes
only partially (kind='partial'). One row per release group, acting for the
recorded requester (D7).
"""

import msgspec


class WantedRetrying(msgspec.Struct):
    """A request still in its auto-retry ladder - shown read-only on the Wanted
    tab so 'is the app still trying?' has one answer (owner decision 2026-07-06).
    Not a watch: the watcher takes over only when the ladder exhausts."""

    release_group_mbid: str
    artist_name: str
    album_title: str
    retry_count: int  # retries already spent; the upcoming one is retry_count + 1
    max_attempts: int
    next_retry_at: float
    artist_mbid: str | None = None
    year: int | None = None
    cover_url: str | None = None
    user_id: str | None = None


class WantedWatch(msgspec.Struct):
    release_group_mbid: str
    user_id: str
    artist_name: str
    album_title: str
    kind: str  # 'missing' | 'partial'
    state: str  # 'watching' | 'dormant' | 'stopped' | 'fulfilled'
    created_at: float
    next_check_at: float
    artist_mbid: str | None = None
    year: int | None = None
    cover_url: str | None = None
    first_release_date: str | None = None  # MB partial date; None = treat as old
    check_count: int = 0
    quiet_streak: int = 0  # consecutive no_results/seen_only cycles (cadence doubling)
    last_checked_at: float | None = None
    last_outcome: str | None = None
    new_candidate_count: int = 0
