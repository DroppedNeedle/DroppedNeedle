import hashlib
from typing import Optional
from fastapi import APIRouter, HTTPException, Path, Query, Depends, Request
from fastapi.responses import Response
from core.dependencies import get_coverart_repository
from infrastructure.msgspec_fastapi import MsgSpecRoute
from repositories.coverart_repository import CoverArtRepository

router = APIRouter(route_class=MsgSpecRoute, prefix="/covers", tags=["covers"])

_ALLOWED_SIZES = {"250", "500", "1200"}
_SIZE_ALIAS_NONE = {"", "original", "full", "max", "largest"}

# Placeholders are cached only briefly (not a day): a cover that misses on a cold visit is
# warmed in the background, and a long-lived placeholder in the browser cache would mask the
# real cover for that whole window. 5 minutes lets it fill in on the next visit while still
# sparing the server a placeholder request on every scroll.
_PLACEHOLDER_CACHE_CONTROL = "public, max-age=300"


def _warming_response() -> Response:
    """A cover whose real art is being resolved in the background right now. 202 + empty body
    makes the browser's <img> fire onerror, which the frontend's image component treats as
    'still warming' - it holds a shimmer skeleton and polls until the cover lands, rather than
    settling on the placeholder. no-store so each poll actually re-requests."""
    return Response(
        status_code=202,
        content=b"",
        headers={"Cache-Control": "no-store", "X-Cover-Source": "warming"},
    )


def _quote_etag(content_hash: str) -> str:
    return f'"{content_hash}"'


def _etag_matches(if_none_match: Optional[str], etag_header: str) -> bool:
    if not if_none_match:
        return False

    candidates = [token.strip() for token in if_none_match.split(",")]
    if "*" in candidates:
        return True

    if etag_header in candidates:
        return True

    weak_etag = f"W/{etag_header}"
    return weak_etag in candidates


def _normalize_size(size: Optional[str]) -> Optional[str]:
    if size is None:
        return "500"
    normalized = size.strip().lower()
    if normalized in _SIZE_ALIAS_NONE:
        return None
    if normalized not in _ALLOWED_SIZES:
        raise HTTPException(
            status_code=400,
            detail=f"Unsupported size '{size}'. Choose one of 250, 500, 1200 or original.",
        )
    return normalized


@router.get("/release-group/{release_group_id}")
async def cover_from_release_group(
    request: Request,
    release_group_id: str = Path(..., min_length=1, description="MusicBrainz release group ID"),
    size: Optional[str] = Query(
        "500",
        description="Preferred size: 250, 500, 1200, or 'original' for full size",
    ),
    coverart_repo: CoverArtRepository = Depends(get_coverart_repository)
):
    desired_size = _normalize_size(size)

    etag_hash = await coverart_repo.get_release_group_cover_etag(release_group_id, desired_size)
    etag_header = _quote_etag(etag_hash) if etag_hash else None
    if etag_header and _etag_matches(request.headers.get("if-none-match"), etag_header):
        return Response(
            status_code=304,
            headers={
                "Cache-Control": "public, max-age=31536000, immutable",
                "ETag": etag_header,
            },
        )

    # defer_best_release: skip the expensive two-call CAA best-release fallback on the request
    # and resolve it in the background. While that runs we return 202 (warming) so the frontend
    # holds a skeleton and polls the cover in live. Compat/streaming clients call the repo
    # directly without this flag and keep the full inline path.
    result = await coverart_repo.get_release_group_cover(release_group_id, desired_size, is_disconnected=request.is_disconnected, defer_best_release=True)

    if result:
        image_data, content_type, source = result
        if not etag_header:
            etag_header = _quote_etag(hashlib.sha1(image_data).hexdigest())
        return Response(
            content=image_data,
            media_type=content_type,
            headers={
                "Cache-Control": "public, max-age=31536000, immutable",
                "X-Cover-Source": source,
                "ETag": etag_header,
            }
        )

    if coverart_repo.is_rg_cover_warming(release_group_id, desired_size):
        return _warming_response()

    placeholder_svg = '''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">
        <rect fill="#374151" width="200" height="200"/>
        <circle cx="100" cy="100" r="70" fill="#1f2937" stroke="#4B5563" stroke-width="2"/>
        <circle cx="100" cy="100" r="50" fill="none" stroke="#4B5563" stroke-width="1"/>
        <circle cx="100" cy="100" r="30" fill="none" stroke="#4B5563" stroke-width="1"/>
        <circle cx="100" cy="100" r="12" fill="#4B5563"/>
        <circle cx="100" cy="100" r="4" fill="#374151"/>
    </svg>'''
    return Response(
        content=placeholder_svg.encode(),
        media_type="image/svg+xml",
        headers={
            "Cache-Control": _PLACEHOLDER_CACHE_CONTROL,
            "X-Cover-Source": "placeholder",
        }
    )


@router.get("/release/{release_id}")
async def cover_from_release(
    request: Request,
    release_id: str = Path(..., min_length=1, description="MusicBrainz release ID"),
    size: Optional[str] = Query(
        "500",
        description="Preferred size: 250, 500, 1200, or 'original' for full size",
    ),
    coverart_repo: CoverArtRepository = Depends(get_coverart_repository)
):
    desired_size = _normalize_size(size)

    etag_hash = await coverart_repo.get_release_cover_etag(release_id, desired_size)
    etag_header = _quote_etag(etag_hash) if etag_hash else None
    if etag_header and _etag_matches(request.headers.get("if-none-match"), etag_header):
        return Response(
            status_code=304,
            headers={
                "Cache-Control": "public, max-age=31536000, immutable",
                "ETag": etag_header,
            },
        )

    result = await coverart_repo.get_release_cover(release_id, desired_size, is_disconnected=request.is_disconnected)
    
    if result:
        image_data, content_type, source = result
        if not etag_header:
            etag_header = _quote_etag(hashlib.sha1(image_data).hexdigest())
        return Response(
            content=image_data,
            media_type=content_type,
            headers={
                "Cache-Control": "public, max-age=31536000, immutable",
                "X-Cover-Source": source,
                "ETag": etag_header,
            }
        )
    
    placeholder_svg = '''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">
        <rect fill="#374151" width="200" height="200"/>
        <circle cx="100" cy="100" r="70" fill="#1f2937" stroke="#4B5563" stroke-width="2"/>
        <circle cx="100" cy="100" r="50" fill="none" stroke="#4B5563" stroke-width="1"/>
        <circle cx="100" cy="100" r="30" fill="none" stroke="#4B5563" stroke-width="1"/>
        <circle cx="100" cy="100" r="12" fill="#4B5563"/>
        <circle cx="100" cy="100" r="4" fill="#374151"/>
    </svg>'''
    return Response(
        content=placeholder_svg.encode(),
        media_type="image/svg+xml",
        headers={
            "Cache-Control": _PLACEHOLDER_CACHE_CONTROL,
            "X-Cover-Source": "placeholder",
        }
    )


@router.get("/artist/{artist_id}")
async def get_artist_cover(
    request: Request,
    artist_id: str,
    size: Optional[int] = Query(None, description="Preferred size in pixels for width"),
    coverart_repo: CoverArtRepository = Depends(get_coverart_repository)
):
    etag_hash = await coverart_repo.get_artist_image_etag(artist_id, size)
    etag_header = _quote_etag(etag_hash) if etag_hash else None
    if etag_header and _etag_matches(request.headers.get("if-none-match"), etag_header):
        return Response(
            status_code=304,
            headers={
                "Cache-Control": "public, max-age=31536000, immutable",
                "ETag": etag_header,
            },
        )

    # defer_wikidata: keep the MusicBrainz->Wikidata->Commons chain (MusicBrainz 1/s) off the
    # request. AudioDB cache + local art still resolve inline; a cold artist returns 202
    # (warming) while the image resolves in the background, and the frontend polls it in live.
    result = await coverart_repo.get_artist_image(artist_id, size, is_disconnected=request.is_disconnected, defer_wikidata=True)

    if not result:
        if coverart_repo.is_artist_cover_warming(artist_id, size):
            return _warming_response()
        placeholder_svg = '''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">
            <rect fill="#374151" width="200" height="200"/>
            <circle cx="100" cy="80" r="30" fill="#6B7280"/>
            <path d="M60 120 Q100 140 140 120 L140 160 Q100 180 60 160 Z" fill="#6B7280"/>
        </svg>'''
        return Response(
            content=placeholder_svg.encode(),
            media_type="image/svg+xml",
            headers={
                "Cache-Control": _PLACEHOLDER_CACHE_CONTROL,
                "X-Cover-Source": "placeholder",
            }
        )
    
    image_data, content_type, source = result
    if not etag_header:
        etag_header = _quote_etag(hashlib.sha1(image_data).hexdigest())
    return Response(
        content=image_data,
        media_type=content_type,
        headers={
            "Cache-Control": "public, max-age=31536000, immutable",
            "X-Cover-Source": source,
            "ETag": etag_header,
        }
    )


@router.get("/debug/artist/{artist_id}")
async def debug_artist_cover(
    artist_id: str,
    coverart_repo: CoverArtRepository = Depends(get_coverart_repository)
):
    """
    Debug endpoint that returns diagnostic info about an artist image fetch.
    Shows cache state, Lidarr availability, MusicBrainz relations, and Wikidata URL.
    """
    from infrastructure.validators import validate_mbid
    
    debug_info = {
        "artist_id": artist_id,
        "is_valid_mbid": False,
        "validated_mbid": None,
        "disk_cache": {
            "exists_250": False,
            "exists_500": False,
            "negative_250": False,
            "negative_500": False,
            "meta_250": None,
            "meta_500": None,
        },
        "library": {
            "configured": False,
            "has_image_url": False,
            "image_url": None,
        },
        "musicbrainz": {
            "artist_found": False,
            "has_wikidata_relation": False,
            "wikidata_url": None,
        },
        "memory_cache": {
            "wikidata_url_cached": False,
            "cached_value": None,
        },
        "circuit_breakers": {},
        "recommendation": None,
    }
    
    try:
        validated_id = validate_mbid(artist_id, "artist")
        debug_info["is_valid_mbid"] = True
        debug_info["validated_mbid"] = validated_id
    except ValueError as e:
        debug_info["recommendation"] = f"Invalid MBID format: {e}. No image can be fetched."
        return debug_info
    
    debug_info = await coverart_repo.debug_artist_image(validated_id, debug_info)
    
    if debug_info["disk_cache"]["negative_250"] or debug_info["disk_cache"]["negative_500"]:
        debug_info["recommendation"] = "Artist has a negative cache entry. Wait for expiry or purge negative cache."
    elif debug_info["disk_cache"]["exists_250"] or debug_info["disk_cache"]["exists_500"]:
        debug_info["recommendation"] = "Image is cached on disk - should load successfully."
    elif any(
        breaker.get("state") == "open"
        for breaker in debug_info.get("circuit_breakers", {}).values()
        if isinstance(breaker, dict)
    ):
        debug_info["recommendation"] = "One or more cover fetch circuit breakers are OPEN. Retry after cooldown or reset breakers."
    elif debug_info["library"]["has_image_url"]:
        debug_info["recommendation"] = "Lidarr has an image URL - fetch should succeed from Lidarr."
    elif debug_info["musicbrainz"]["has_wikidata_relation"]:
        debug_info["recommendation"] = "Wikidata URL found - fetch should succeed from Wikidata/Wikimedia."
    else:
        debug_info["recommendation"] = "No image source found. This artist will show a placeholder."
    
    return debug_info
