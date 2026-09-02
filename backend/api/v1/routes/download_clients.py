"""Download-clients (SABnzbd) + shared download-policy admin routes.

SABnzbd config GET/PUT/test and the shared ``download_policy`` GET/PUT are admin-only.
The SABnzbd ``api_key`` is the FULL key (the add-only nzbkey can't do queue/history/
delete) - masked on GET, preserved on PUT when the masked sentinel comes back. Test
reports SABnzbd's version + category list + completed dir (the mount hint).
"""

import logging


import msgspec
from fastapi import APIRouter, Depends

from api.v1.schemas.download import (
    PolicyImpactResponse,
    PolicySummaryResponse,
    SabnzbdTestResponse,
    SourcePriority,
)
from api.v1.schemas.settings import (
    SABNZBD_API_KEY_MASK,
    DownloadPolicySettings,
    SabnzbdConnectionSettings,
    WantedWatcherSettings,
    validate_new_quality_fields,
)
from core.dependencies import build_sabnzbd_download_client, get_preferences_service
from core.exceptions import ExternalServiceError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep, CurrentUserDep

logger = logging.getLogger(__name__)

router = APIRouter(
    route_class=MsgSpecRoute, prefix="/download-clients", tags=["download-clients"]
)


def _clear_download_client_cache() -> None:
    # Both the SABnzbd connection and the shared policy feed the scorers, file processor,
    # orchestrator and service - clear the whole chain so a save takes effect at once.
    from core.dependencies import (
        get_album_preflight_scorer,
        get_download_orchestrator,
        get_download_service,
        get_file_processor,
        get_newznab_indexer,
        get_newznab_release_scorer,
        get_sabnzbd_client,
        get_sabnzbd_download_client,
        get_track_matcher,
        get_target_download_orchestrator,
        get_target_download_service,
        get_target_file_processor,
    )

    for provider in (
        get_sabnzbd_client,
        get_sabnzbd_download_client,
        get_album_preflight_scorer,
        get_track_matcher,
        get_newznab_release_scorer,
        # the indexer derives its search-cache TTL from the policy's auto-retry interval
        get_newznab_indexer,
        get_file_processor,
        get_download_orchestrator,
        get_download_service,
        get_target_file_processor,
        get_target_download_orchestrator,
        get_target_download_service,
    ):
        provider.cache_clear()


@router.get("/sabnzbd", response_model=SabnzbdConnectionSettings)
async def get_sabnzbd(_: CurrentAdminDep, preferences=Depends(get_preferences_service)):
    return preferences.get_sabnzbd_connection()


@router.put("/sabnzbd", response_model=SabnzbdConnectionSettings)
async def update_sabnzbd(
    _: CurrentAdminDep,
    settings: SabnzbdConnectionSettings = MsgSpecBody(SabnzbdConnectionSettings),
    preferences=Depends(get_preferences_service),
):
    preferences.save_sabnzbd_connection(settings)
    _clear_download_client_cache()
    return preferences.get_sabnzbd_connection()


@router.post("/sabnzbd/test", response_model=SabnzbdTestResponse)
async def test_sabnzbd(
    _: CurrentAdminDep,
    settings: SabnzbdConnectionSettings = MsgSpecBody(SabnzbdConnectionSettings),
    preferences=Depends(get_preferences_service),
):
    """Tests the submitted url/key (not stored config). A masked key resolves to the
    stored one."""
    api_key = settings.api_key
    if api_key == SABNZBD_API_KEY_MASK:
        api_key = preferences.get_sabnzbd_connection_raw().api_key

    client = build_sabnzbd_download_client(settings.url, api_key, settings.downloads_mount)
    try:
        status = await client.health_check()
        if status.status != "ok":
            return SabnzbdTestResponse(
                valid=False, message=status.message or "SABnzbd unreachable"
            )
        cats = await client.get_categories()
        complete_dir = await client.get_complete_dir()
    except ExternalServiceError as exc:
        return SabnzbdTestResponse(valid=False, message=str(exc))
    # Catch a misconfigured downloads mount at config time (rokim's class of
    # report): the submitted mount - not the stored one - is diagnosed, so an
    # unsaved correction already shows the fixed verdict.
    diagnosis = await client.diagnose_downloads_mount()
    mount_message = None
    if (
        diagnosis.sampled_downloads > 0
        and diagnosis.resolvable_downloads < diagnosis.sampled_downloads
    ):
        mount_message = (
            f"Only {diagnosis.resolvable_downloads}/{diagnosis.sampled_downloads} "
            f"sampled SABnzbd download(s) resolve under {settings.downloads_mount} - "
            "the mount likely points at the wrong folder (for example a category "
            "subfolder of SABnzbd's completed dir shown above)"
        )
    return SabnzbdTestResponse(
        valid=True,
        version=status.version,
        message=f"SABnzbd {status.version}",
        categories=cats,
        complete_dir=complete_dir or None,
        mount_has_files=diagnosis.mount_has_files,
        resolvable_downloads=diagnosis.resolvable_downloads,
        sampled_downloads=diagnosis.sampled_downloads,
        mount_message=mount_message,
    )


@router.get("/source-priority", response_model=SourcePriority)
async def get_source_priority(
    _: CurrentAdminDep, preferences=Depends(get_preferences_service)
):
    return SourcePriority(order=preferences.get_source_priority())


@router.put("/source-priority", response_model=SourcePriority)
async def update_source_priority(
    _: CurrentAdminDep,
    body: SourcePriority = MsgSpecBody(SourcePriority),
    preferences=Depends(get_preferences_service),
):
    preferences.save_source_priority(body.order)
    _clear_download_client_cache()
    return SourcePriority(order=preferences.get_source_priority())


@router.get("/policy", response_model=DownloadPolicySettings)
async def get_policy(_: CurrentAdminDep, preferences=Depends(get_preferences_service)):
    return preferences.get_download_policy()


@router.put("/policy", response_model=DownloadPolicySettings)
async def update_policy(
    _: CurrentAdminDep,
    preferences=Depends(get_preferences_service),
    payload: dict = MsgSpecBody(dict),
):
    # Validate the RAW body BEFORE struct construction: DownloadPolicySettings'
    # __post_init__ heals stored-data drift, which would silently clamp a
    # submitted field (Acquisition plan: never clamp new-field submissions).
    try:
        validate_new_quality_fields(payload)
        policy = msgspec.convert(payload, type=DownloadPolicySettings)
    except (ValueError, TypeError, msgspec.ValidationError) as exc:
        from core.exceptions import ValidationError

        raise ValidationError(str(exc)) from exc
    preferences.save_download_policy(policy)
    _clear_download_client_cache()
    return preferences.get_download_policy()


@router.get("/policy-summary", response_model=PolicySummaryResponse)
async def get_policy_summary(
    current_user: CurrentUserDep,
    preferences=Depends(get_preferences_service),
):
    """Safe read-only acquisition-policy summary for ANY signed-in user (spec):
    the backend-composed contract sentence plus the source-mode label only - no
    admin resilience/cap internals."""
    from services.native.acquisition.quality import (
        build_snapshot,
        derive_default_order,
    )

    policy = preferences.get_download_policy()
    snapshot = build_snapshot(policy)
    order = snapshot.quality_preference_order
    derived = derive_default_order(policy.quality_min, policy.quality_max)
    legacy_rollback_compatible = (
        order == derived
        and policy.lossless_preference == "highest"
        and policy.lossless_max_bit_depth is None
        and policy.lossless_max_sample_rate_hz is None
        and policy.preferred_lossy_bitrate_kbps is None
        and policy.lossy_min_bitrate_kbps is None
        and policy.lossy_max_bitrate_kbps is None
        and policy.unknown_quality_behavior == "allow_as_fallback"
    )
    return PolicySummaryResponse(
        summary=snapshot.summary,
        source_mode=policy.source_selection_mode,
        legacy_rollback_compatible=(
            not policy.quality_recipe and legacy_rollback_compatible
        ),
        quality_recipe_status=policy.quality_recipe_status,
        quality_recipe_error=policy.quality_recipe_error,
    )


@router.post("/policy/impact", response_model=PolicyImpactResponse)
async def preview_policy_impact(
    _: CurrentAdminDep,
    preferences=Depends(get_preferences_service),
    payload: dict = MsgSpecBody(dict),
):
    """Admin impact preview of an UNSAVED policy body against persisted rows
    (spec): persisted-state bucket counts only; the PUT /policy response shape
    is untouched. Shares the strict raw-payload validation with the save path."""
    from core.dependencies.repo_providers import get_download_store
    from services.native.acquisition.quality import (
        build_snapshot,
        derive_default_order,
    )

    try:
        validate_new_quality_fields(payload)
        candidate = msgspec.convert(payload, type=DownloadPolicySettings)
        snapshot = build_snapshot(candidate)
    except (ValueError, TypeError, msgspec.ValidationError) as exc:
        from core.exceptions import ValidationError

        raise ValidationError(str(exc)) from exc
    # Legacy-representable = EXACTLY today's default shape (hi-res-first over a
    # contiguous range with no custom targets/caps/evidence overrides) - i.e.
    # what an older image can reproduce on load.
    legacy_representable = (
        sorted(snapshot.quality_preference_order)
        == sorted(derive_default_order(candidate.quality_min, candidate.quality_max))
        and snapshot.quality_preference_order
        == derive_default_order(candidate.quality_min, candidate.quality_max)
        and candidate.lossless_preference == "highest"
        and candidate.lossless_max_bit_depth is None
        and candidate.lossless_max_sample_rate_hz is None
        and candidate.preferred_lossy_bitrate_kbps is None
        and candidate.lossy_min_bitrate_kbps is None
        and candidate.lossy_max_bitrate_kbps is None
        and candidate.unknown_quality_behavior == "allow_as_fallback"
        and candidate.source_selection_mode == "source_first"
    )
    buckets = await get_download_store().acquisition_policy_impact()
    return PolicyImpactResponse(
        manual_search_jobs=buckets["manual_search_jobs"],
        queued_without_attempts=buckets["queued_without_attempts"],
        awaiting_review=buckets["awaiting_review"],
        remote_queued_zero_byte=buckets["remote_queued_zero_byte"],
        transferring_immutable=buckets["transferring"],
        held_reviews=buckets["held_reviews"],
        legacy_representable=bool(legacy_representable),
    )


@router.get("/wanted", response_model=WantedWatcherSettings)
async def get_wanted_watcher_settings(
    _: CurrentAdminDep, preferences=Depends(get_preferences_service)
):
    return preferences.get_wanted_settings()


@router.put("/wanted", response_model=WantedWatcherSettings)
async def update_wanted_watcher_settings(
    _: CurrentAdminDep,
    settings: WantedWatcherSettings = MsgSpecBody(WantedWatcherSettings),
    preferences=Depends(get_preferences_service),
):
    # no singleton clearing needed: the watcher re-reads this section every sweep
    preferences.save_wanted_settings(settings)
    return preferences.get_wanted_settings()
