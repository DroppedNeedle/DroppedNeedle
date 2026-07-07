"""Newznab indexer admin routes (D6): list / create / update / delete / reorder /
test. All admin-only. ``api_key`` is masked on GET and preserved on PUT when the
masked sentinel comes back.

DroppedNeedle bundles no indexers - the list starts empty and the user adds their
own Generic Newznab endpoint (guardrail 1). Test reports caps/version + whether the
indexer advertises ``<audio-search>`` (so the user knows whether structured music
search or the ``t=search`` fallback will be used).
"""

import logging
from urllib.parse import urlsplit

from fastapi import APIRouter, Depends

from api.v1.schemas.download import (
    IndexerReorderRequest,
    IndexerSavedResponse,
    IndexerTestResponse,
    OperationResult,
)
from api.v1.schemas.settings import INDEXER_API_KEY_MASK, NewznabIndexerSettings
from core.dependencies import build_newznab_client, get_preferences_service
from core.exceptions import ExternalServiceError, NewznabAuthError, RateLimitedError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/indexers", tags=["indexers"])


def _clear_indexer_cache() -> None:
    # The orchestrator/service/scorer capture the indexer at construction, so clear the
    # whole downstream chain - else an added/removed indexer doesn't take effect.
    from core.dependencies import (
        get_download_orchestrator,
        get_download_service,
        get_newznab_indexer,
        get_newznab_release_scorer,
    )

    for provider in (
        get_newznab_indexer,
        get_newznab_release_scorer,
        get_download_orchestrator,
        get_download_service,
    ):
        provider.cache_clear()


def _api_path_suggestion(url: str) -> str | None:
    """A bare site URL (no path) is almost never the newznab endpoint itself - the
    API lives at ``/api`` (nzbgeek, ninjacentral, DrunkenSlug, and the *arr "Generic
    Newznab" convention all do this). Offer ``<url>/api`` only when the path is empty."""
    if urlsplit(url).path.strip("/"):
        return None
    return f"{url.rstrip('/')}/api"


async def _reaches_newznab(url: str, api_key: str) -> bool:
    """Did ``url`` answer as a real newznab endpoint? A caps hit, an auth error, or a
    rate-limit all mean we reached the API; an HTML page / unreachable host raises a
    plain NewznabApiError. Used to confirm a ``/api`` suggestion before offering it."""
    try:
        await build_newznab_client(url, api_key).caps()
    except (NewznabAuthError, RateLimitedError):
        return True
    except ExternalServiceError:
        return False
    return True


@router.get("", response_model=list[NewznabIndexerSettings])
async def list_indexers(_: CurrentAdminDep, preferences=Depends(get_preferences_service)):
    return preferences.get_indexers()


@router.post("", response_model=IndexerSavedResponse)
async def create_indexer(
    _: CurrentAdminDep,
    settings: NewznabIndexerSettings = MsgSpecBody(NewznabIndexerSettings),
    preferences=Depends(get_preferences_service),
):
    indexer_id = preferences.save_indexer(settings)
    _clear_indexer_cache()
    return IndexerSavedResponse(id=indexer_id)


@router.put("/{indexer_id}", response_model=IndexerSavedResponse)
async def update_indexer(
    indexer_id: str,
    _: CurrentAdminDep,
    settings: NewznabIndexerSettings = MsgSpecBody(NewznabIndexerSettings),
    preferences=Depends(get_preferences_service),
):
    settings.id = indexer_id  # the path is authoritative
    saved_id = preferences.save_indexer(settings)
    _clear_indexer_cache()
    return IndexerSavedResponse(id=saved_id)


@router.delete("/{indexer_id}", response_model=OperationResult)
async def delete_indexer(
    indexer_id: str,
    _: CurrentAdminDep,
    preferences=Depends(get_preferences_service),
):
    preferences.delete_indexer(indexer_id)
    _clear_indexer_cache()
    return OperationResult(success=True)


@router.post("/reorder", response_model=OperationResult)
async def reorder_indexers(
    _: CurrentAdminDep,
    body: IndexerReorderRequest = MsgSpecBody(IndexerReorderRequest),
    preferences=Depends(get_preferences_service),
):
    preferences.reorder_indexers(body.ordered_ids)
    _clear_indexer_cache()
    return OperationResult(success=True)


@router.post("/test", response_model=IndexerTestResponse)
async def test_indexer(
    _: CurrentAdminDep,
    settings: NewznabIndexerSettings = MsgSpecBody(NewznabIndexerSettings),
    preferences=Depends(get_preferences_service),
):
    """Tests the caps endpoint with the submitted url/key (not stored config), so Test
    works before the first save. A masked key resolves to the stored one."""
    api_key = settings.api_key
    if api_key == INDEXER_API_KEY_MASK:
        stored = next((i for i in preferences.get_indexers_raw() if i.id == settings.id), None)
        api_key = stored.api_key if stored else ""

    client = build_newznab_client(settings.url, api_key)
    try:
        caps = await client.caps()
    except ExternalServiceError as exc:
        suggestion = _api_path_suggestion(settings.url)
        if suggestion and await _reaches_newznab(suggestion, api_key):
            return IndexerTestResponse(
                valid=False,
                message="That's the site's homepage, not the API endpoint.",
                suggested_url=suggestion,
            )
        return IndexerTestResponse(valid=False, message=str(exc))
    return IndexerTestResponse(
        valid=True,
        version=caps.server_version,
        message=(
            f"{caps.server_title or 'Indexer'} OK - "
            + ("structured music search" if caps.supports_audio_search else "text search (no audio-search)")
        ),
        supports_audio_search=caps.supports_audio_search,
        category_count=len(caps.categories),
    )
