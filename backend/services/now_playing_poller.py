"""Background loop that folds upstream Jellyfin/Navidrome/Plex sessions into the
now-playing presence feed, and sweeps stale native/compat sessions.

Kept separate from ``NowPlayingService`` (which owns state) so the polling/mapping is
unit-testable in isolation. Each source is reconciled independently and defensively:
a failing or unconfigured integration never breaks the cycle or the other sources.
"""

import asyncio
import logging

from services.now_playing_service import ExternalSession, NowPlayingService

logger = logging.getLogger(__name__)

POLL_INTERVAL_SECONDS = 4.0


def map_jellyfin(resp) -> list[ExternalSession]:
    out: list[ExternalSession] = []
    for s in resp.sessions:
        if not s.track_name:
            continue
        out.append(
            ExternalSession(
                key=f"jellyfin:{s.session_id}",
                user_name=s.user_name,
                device_name=s.device_name or s.client_name,
                track_name=s.track_name,
                artist_name=s.artist_name,
                album_name=s.album_name or None,
                cover_url=s.cover_url,
                is_paused=s.is_paused,
                progress_ms=int(s.position_seconds * 1000),
                duration_ms=int(s.duration_seconds * 1000),
            )
        )
    return out


def map_navidrome(resp) -> list[ExternalSession]:
    out: list[ExternalSession] = []
    for e in resp.entries:
        if not e.track_name:
            continue
        if e.estimated_position_seconds and e.estimated_position_seconds > 0:
            progress = int(e.estimated_position_seconds * 1000)
        elif e.minutes_ago > 0:
            progress = max(0, e.duration_seconds * 1000 - e.minutes_ago * 60_000)
        else:
            progress = 0
        out.append(
            ExternalSession(
                key=f"navidrome:{e.user_name}:{e.player_name}:{e.album_id}:{e.track_name}",
                user_name=e.user_name,
                device_name=e.player_name,
                track_name=e.track_name,
                artist_name=e.artist_name,
                album_name=e.album_name or None,
                # Navidrome's getNowPlaying carries no play/pause state
                cover_url=f"/api/v1/navidrome/cover/{e.cover_art_id}" if e.cover_art_id else "",
                is_paused=False,
                progress_ms=progress,
                duration_ms=e.duration_seconds * 1000,
            )
        )
    return out


def map_plex(resp) -> list[ExternalSession]:
    out: list[ExternalSession] = []
    for s in resp.sessions:
        if not s.track_title:
            continue
        out.append(
            ExternalSession(
                key=f"plex:{s.session_id}",
                user_name=s.user_name,
                device_name=s.player_device,
                track_name=s.track_title,
                artist_name=s.artist_name,
                album_name=s.album_name or None,
                cover_url=s.cover_url,
                is_paused=s.player_state == "paused",
                progress_ms=s.progress_ms,
                duration_ms=s.duration_ms,
            )
        )
    return out


async def poll_external_once(
    now_playing: NowPlayingService,
    home_service,
    jellyfin_service,
    navidrome_service,
    plex_service,
) -> None:
    """Run one reconcile cycle across the three upstream integrations."""
    status = home_service.get_integration_status()

    async def _reconcile(source, enabled, fetch, mapper):
        if not enabled:
            await now_playing.reconcile_source(source, [])
            return
        try:
            resp = await fetch()
            await now_playing.reconcile_source(source, mapper(resp))
        except Exception as e:  # noqa: BLE001 - one bad source must not break the cycle
            logger.debug("now-playing %s poll failed: %s", source, e)

    await _reconcile(
        "jellyfin", getattr(status, "jellyfin", False), jellyfin_service.get_sessions, map_jellyfin
    )
    await _reconcile(
        "navidrome",
        getattr(status, "navidrome", False),
        navidrome_service.get_now_playing,
        map_navidrome,
    )
    await _reconcile(
        "plex", getattr(status, "plex", False), plex_service.get_sessions, map_plex
    )


async def run_now_playing_presence_loop(
    now_playing: NowPlayingService,
    home_service,
    jellyfin_service,
    navidrome_service,
    plex_service,
    interval: float = POLL_INTERVAL_SECONDS,
) -> None:
    while True:
        try:
            await now_playing.sweep()
            await poll_external_once(
                now_playing, home_service, jellyfin_service, navidrome_service, plex_service
            )
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.warning("now-playing presence loop cycle failed: %s", e)
        await asyncio.sleep(interval)
