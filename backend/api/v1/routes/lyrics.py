"""Metadata-only lyrics lookup - ``GET /api/v1/lyrics/lookup``.

The Plex-playback lyrics path: Plex streams have no DroppedNeedle library file,
so the player looks lyrics up by artist/track (+ optional album/duration)
against LRCLIB. Gated on the ``lyrics_fetch_enabled`` library setting; when the
setting is off the route answers 404, which the frontend already treats as
"no lyrics" (same contract as the library/jellyfin/navidrome lyrics routes).
"""

from fastapi import APIRouter, Depends, HTTPException, Query

from api.v1.schemas.library import LibraryLyricsResponse
from core.dependencies import get_lyrics_lookup_service
from infrastructure.msgspec_fastapi import MsgSpecRoute
from middleware import CurrentUserDep
from services.lyrics_lookup_service import LyricsLookupService

router = APIRouter(route_class=MsgSpecRoute, prefix="/lyrics", tags=["lyrics"])


@router.get("/lookup", response_model=LibraryLyricsResponse)
async def lookup_lyrics(
    current_user: CurrentUserDep,
    artist: str = Query(..., min_length=1),
    track: str = Query(..., min_length=1),
    album: str | None = Query(None),
    duration: float | None = Query(None, ge=0),
    service: LyricsLookupService = Depends(get_lyrics_lookup_service),
) -> LibraryLyricsResponse:
    """Lyrics by track signature (no local file). 404 = no lyrics - both when
    the admin fetch setting is off and on an LRCLIB miss."""
    if not service.enabled():
        raise HTTPException(status_code=404, detail="Lyrics not available")
    lyrics = await service.lookup(
        artist=artist, track=track, album=album, duration_seconds=duration
    )
    if lyrics is None:
        raise HTTPException(status_code=404, detail="Lyrics not available")
    return lyrics
