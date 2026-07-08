SHELL := /bin/bash

.DEFAULT_GOAL := help

ROOT_DIR     := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
BACKEND_DIR  := $(ROOT_DIR)/backend
FRONTEND_DIR := $(ROOT_DIR)/frontend

BACKEND_VENV_DIR   := $(BACKEND_DIR)/.venv
BACKEND_VENV_STAMP := $(BACKEND_VENV_DIR)/.deps-stamp
PYTEST := cd "$(BACKEND_DIR)" && .venv/bin/python -m pytest

PYTHON ?= python3
NPM    ?= pnpm

.PHONY: \
	help \
	backend-venv backend-lint backend-test \
	test-compat backend-test-compat \
	backend-test-compat-subsonic backend-test-compat-jellyfin backend-test-compat-streaming \
	frontend-test-connect-apps \
	backend-test-album-refresh \
	backend-test-artist-monitoring \
	backend-test-artist-page \
	backend-test-local-stats \
	backend-test-artist-discovery \
	backend-test-audiodb \
	backend-test-audiodb-parallel \
	backend-test-audiodb-phase8 \
	backend-test-audiodb-phase9 \
	backend-test-audiodb-prewarm \
	backend-test-audiodb-settings \
	backend-test-cache-cleanup \
	backend-test-wanted \
	test-events \
	backend-test-config-validation \
	backend-test-coverart-audiodb \
	backend-test-dedup-cancellation \
	backend-test-discovery \
	backend-test-discover-schemas \
	backend-test-daily-mix \
	backend-test-discover-picks \
	backend-test-discover-radio \
	backend-test-playlist-suggestions \
	backend-test-unexplored-genres \
	backend-test-deep-discovery \
	backend-test-discovery-precache \
	backend-test-exception-handling \
	backend-test-genre-index \
	backend-test-home \
	backend-test-now-playing \
	backend-test-scrobble \
	backend-test-connections \
	backend-test-home-genre \
	backend-test-infra-hardening \
	backend-test-memory \
	backend-test-jellyfin \
	backend-test-jellyfin-proxy \
	backend-test-library-pagination \
	backend-test-artists-aggregated-pagination \
	backend-test-audio-tagger \
	backend-test-naming-template-engine \
	backend-test-library-manager \
	backend-test-tracks-view \
	backend-test-library-scanner \
	backend-test-scan-scheduler \
	backend-test-audio-fingerprinter \
	backend-test-musicbrainz-matcher \
	backend-test-sse-publisher \
	backend-test-local-files \
	backend-test-monitoring-cache \
	backend-test-navidrome \
	backend-test-multidisc \
	backend-test-performance \
	backend-test-preferences \
	backend-test-plex \
	backend-test-plex-repository \
	backend-test-plex-routes \
	backend-test-playlist \
	backend-test-queue-strategies \
	backend-test-request-service \
	backend-test-search-top-result \
	backend-test-security \
	backend-test-sync-coordinator \
	backend-test-sync-generation \
	backend-test-sync-resume \
	backend-test-sync-watchdog \
	backend-test-content-enrichment \
	backend-test-username-login \
	backend-test-user-import \
	backend-test-peer-review-fixes \
	backend-test-discover-all \
	test-discover-all \
	test-audiodb-all test-mus14-all test-sync-all \
	frontend-install frontend-build frontend-browser-install \
	frontend-format-check frontend-check frontend-lint frontend-test frontend-test-server \
	frontend-test-client frontend-test-connections \
	frontend-test-album-page \
	frontend-test-audiodb-images \
	frontend-test-auth \
	frontend-test-auth-username \
	frontend-test-user-import \
	frontend-test-discover-page \
	frontend-test-jellyfin \
	frontend-test-follow \
	frontend-test-navidrome \
	frontend-test-plex \
	frontend-test-playlist-detail \
	frontend-test-queuehelpers \
	rebuild \
	security-tests e2e e2e-fast docs-check \
	frontend-test-e2e frontend-test-integration-coverage \
	fmt format lint tests test ci

help: ## Show available targets
	@grep -E '^[a-zA-Z0-9_.-]+:.*## ' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*## "}; {printf "%-34s %s\n", $$1, $$2}'

$(BACKEND_VENV_DIR):
	cd "$(BACKEND_DIR)" && test -f .virtualenv.pyz || curl -fsSLo .virtualenv.pyz https://bootstrap.pypa.io/virtualenv.pyz
	cd "$(BACKEND_DIR)" && $(PYTHON) .virtualenv.pyz .venv

$(BACKEND_VENV_STAMP): $(BACKEND_DIR)/requirements.txt $(BACKEND_DIR)/requirements-dev.txt | $(BACKEND_VENV_DIR)
	cd "$(BACKEND_DIR)" && .venv/bin/python -m pip install --upgrade pip setuptools wheel
	cd "$(BACKEND_DIR)" && .venv/bin/python -m pip install -r requirements-dev.txt pytest pytest-asyncio
	touch "$(BACKEND_VENV_STAMP)"

backend-venv: $(BACKEND_VENV_STAMP) ## Create or refresh the backend virtualenv

backend-lint: $(BACKEND_VENV_STAMP) ## Run backend Ruff checks
	cd "$(ROOT_DIR)" && $(BACKEND_VENV_DIR)/bin/ruff check backend

backend-test: $(BACKEND_VENV_STAMP) ## Run all backend tests
	$(PYTEST)

backend-test-compat: $(BACKEND_VENV_STAMP) ## Connect Apps: all compat backend tests (auth, serializers, endpoints, streaming, mapping, errors)
	$(PYTEST) tests/compat -v

backend-test-compat-subsonic: $(BACKEND_VENV_STAMP) ## Connect Apps: Subsonic shim only
	$(PYTEST) tests/compat/test_subsonic_*.py -v

backend-test-compat-jellyfin: $(BACKEND_VENV_STAMP) ## Connect Apps: Jellyfin shim only
	$(PYTEST) tests/compat/test_jellyfin_*.py -v

backend-test-compat-streaming: $(BACKEND_VENV_STAMP) ## Connect Apps: streaming + transcode (ffprobe-gated)
	$(PYTEST) tests/compat/test_subsonic_streaming.py tests/compat/test_jellyfin_stream.py tests/compat/test_transcode_service.py -v

frontend-test-connect-apps: ## Connect Apps: SettingsConnectApps component + data-layer tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project client src/lib/components/settings/SettingsConnectApps.svelte.spec.ts
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/queries/connect-apps

test-compat: backend-test-compat frontend-test-connect-apps ## Connect Apps: full backend + frontend suite

backend-test-album-refresh: $(BACKEND_VENV_STAMP) ## Run album refresh endpoint tests
	$(PYTEST) tests/routes/test_album_refresh.py tests/services/test_navidrome_cache_invalidation.py -v

backend-test-album-owned-release: $(BACKEND_VENV_STAMP) ## Owned album shows the edition on disc, not the largest ranked release
	$(PYTEST) tests/services/test_album_service.py tests/services/test_album_singleflight.py -v

backend-test-local-stats: $(BACKEND_VENV_STAMP) ## Listening Room stats sourced from the library DB (home entry-card parity)
	$(PYTEST) tests/services/test_local_files_service.py tests/test_advanced_settings_roundtrip.py -v

backend-test-artist-monitoring: $(BACKEND_VENV_STAMP) ## Run MUS-15B artist monitoring tests
	$(PYTEST) tests/test_artist_monitoring.py -v

backend-test-artist-page: $(BACKEND_VENV_STAMP) ## Run artist page latency tests (basic route, releases, Last.fm fast path)
	$(PYTEST) tests/routes/test_artist_basic_route.py tests/routes/test_artist_releases_route.py tests/services/test_artist_basic_info.py tests/services/test_top_albums_lastfm_fast.py -v

backend-test-artist-discovery: $(BACKEND_VENV_STAMP) ## Run artist discovery service tests (similar artists, top songs/albums)
	$(PYTEST) tests/services/test_artist_discovery_service.py -v

backend-test-audiodb: $(BACKEND_VENV_STAMP) ## Run focused AudioDB backend tests
	$(PYTEST) tests/repositories/test_audiodb_repository.py tests/infrastructure/test_disk_metadata_cache.py tests/services/test_audiodb_image_service.py tests/services/test_artist_audiodb_population.py tests/services/test_album_audiodb_population.py tests/services/test_audiodb_detail_flows.py tests/services/test_search_audiodb_overlay.py

backend-test-audiodb-parallel: $(BACKEND_VENV_STAMP) ## Run AudioDB parallel prewarm tests
	$(PYTEST) tests/test_audiodb_parallel.py -v

backend-test-audiodb-phase8: $(BACKEND_VENV_STAMP) ## Run AudioDB cross-cutting tests
	$(PYTEST) tests/repositories/test_audiodb_models.py tests/test_audiodb_schema_contracts.py tests/services/test_audiodb_byte_caching_integration.py tests/services/test_audiodb_url_only_integration.py tests/services/test_audiodb_fallback_integration.py tests/services/test_audiodb_negative_cache_expiry.py tests/test_audiodb_killswitch.py tests/test_advanced_settings_roundtrip.py

backend-test-audiodb-phase9: $(BACKEND_VENV_STAMP) ## Run AudioDB observability tests
	$(PYTEST) tests/test_phase9_observability.py

backend-test-audiodb-prewarm: $(BACKEND_VENV_STAMP) ## Run AudioDB prewarm tests
	$(PYTEST) tests/services/test_audiodb_prewarm.py tests/services/test_audiodb_sweep.py tests/services/test_audiodb_browse_queue.py tests/services/test_audiodb_fallback_gating.py tests/services/test_preferences_generic_settings.py

backend-test-audiodb-settings: $(BACKEND_VENV_STAMP) ## Run AudioDB settings tests
	$(PYTEST) tests/test_audiodb_settings.py tests/test_advanced_settings_roundtrip.py tests/routes/test_settings_audiodb_key.py

backend-test-wanted: $(BACKEND_VENV_STAMP) ## Wanted watcher: store, service, loop, prune + quota seams
	$(PYTEST) tests/infrastructure/test_wanted_store.py tests/infrastructure/test_wanted_watcher_task.py tests/services/test_wanted_watcher_service.py tests/infrastructure/test_request_history_prune.py tests/services/test_quota_service.py -v

test-events: $(BACKEND_VENV_STAMP) ## Upcoming Events: store, repos, watcher, loop, routes + frontend data layer & city manager
	$(PYTEST) tests/infrastructure/test_events_store.py tests/infrastructure/test_events_watcher_task.py tests/services/test_events_watcher_service.py tests/repositories/test_ticketmaster_repository.py tests/repositories/test_skiddle_repository.py tests/repositories/test_geocoding_repository.py tests/routes/test_following_concerts_routes.py tests/routes/test_settings_events_routes.py -v
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/queries/following
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project client src/lib/components/following/EventCityManager.svelte.spec.ts src/routes/following/page.svelte.spec.ts

test-lidarr-import: $(BACKEND_VENV_STAMP) ## Lidarr import: repo, store bulk-follow/approval, service (D9), routes, batches + frontend data layer & components
	$(PYTEST) tests/repositories/test_lidarr_import_repository.py tests/infrastructure/test_follow_store_lidarr_import.py tests/services/test_lidarr_import_service.py tests/routes/test_lidarr_import_routes.py tests/routes/test_follow_approval_batches_routes.py tests/test_phase1_brownout.py tests/repositories/test_lidarr_directory_empty.py -v
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/queries/lidarr-import
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project client src/lib/components/settings/SettingsLidarrImport.svelte.spec.ts src/lib/components/following/LidarrImportModal.svelte.spec.ts

backend-test-cache-cleanup: $(BACKEND_VENV_STAMP) ## Run cache cleanup tests
	$(PYTEST) tests/test_cache_cleanup.py -v

backend-test-config-validation: $(BACKEND_VENV_STAMP) ## Run config validation tests
	$(PYTEST) tests/test_config_validation.py

test-acquisition-corpus: $(BACKEND_VENV_STAMP) ## Replay the acquisition scoring corpus (2026-07-05 incident + legit priors)
	$(PYTEST) tests/services/test_acquisition_corpus.py tests/services/test_album_preflight_scorer.py tests/services/test_track_matcher.py tests/services/test_title_match.py -v

backend-test-coverart-audiodb: $(BACKEND_VENV_STAMP) ## Run AudioDB coverart provider tests
	$(PYTEST) tests/repositories/test_coverart_album_fetcher.py tests/repositories/test_coverart_audiodb_provider.py tests/repositories/test_coverart_repository_memory_cache.py tests/services/test_audiodb_byte_caching_integration.py

backend-test-dedup-cancellation: $(BACKEND_VENV_STAMP) ## Run deduplicator cancellation tests
	$(PYTEST) tests/infrastructure/test_dedup_cancellation.py tests/infrastructure/test_disconnect.py -v

backend-test-discovery: $(BACKEND_VENV_STAMP) ## Run discovery service and route tests
	$(PYTEST) tests/services/test_discovery.py tests/routes/test_discovery_routes.py -v

backend-test-discover-home-peruser: $(BACKEND_VENV_STAMP) ## Run Phase 5 per-user discovery/home isolation tests (AMU-8)
	$(PYTEST) tests/test_discover_home_peruser.py -v

backend-test-discovery-precache: $(BACKEND_VENV_STAMP) ## Run artist discovery precache tests
	$(PYTEST) tests/services/test_discovery_precache_progress.py tests/services/test_discovery_precache_lock.py tests/infrastructure/test_retry_non_breaking.py -v

backend-test-discovery-peruser: $(BACKEND_VENV_STAMP) ## Run per-user artist/album discovery resolution tests (configured gate + route user_id)
	$(PYTEST) tests/services/test_artist_discovery_service.py tests/services/test_album_discovery_service.py tests/routes/test_discovery_peruser_routes.py -v

backend-test-exception-handling: $(BACKEND_VENV_STAMP) ## Run exception-handling regressions
	$(PYTEST) tests/routes/test_scrobble_routes.py tests/routes/test_scrobble_settings_routes.py tests/test_error_leakage.py tests/test_background_task_logging.py

backend-test-genre-index: $(BACKEND_VENV_STAMP) ## Run genre index tests
	$(PYTEST) tests/infrastructure/test_genre_index.py -v

backend-test-discover-schemas: $(BACKEND_VENV_STAMP) ## Run discover schema roundtrip tests
	$(PYTEST) tests/schemas/test_discover_schemas.py -v

backend-test-daily-mix: $(BACKEND_VENV_STAMP) ## Run daily mix section builder tests
	$(PYTEST) tests/services/test_daily_mix.py -v

backend-test-discover-picks: $(BACKEND_VENV_STAMP) ## Run discover picks section builder tests
	$(PYTEST) tests/services/test_discover_picks.py -v

backend-test-discover-radio: $(BACKEND_VENV_STAMP) ## Run discover radio tests
	$(PYTEST) tests/services/test_discover_radio.py tests/routes/test_discover_radio_routes.py -v

backend-test-playlist-suggestions: $(BACKEND_VENV_STAMP) ## Run playlist suggestion tests
	$(PYTEST) tests/services/test_playlist_suggestions.py tests/routes/test_playlist_suggestions_routes.py -v

backend-test-unexplored-genres: $(BACKEND_VENV_STAMP) ## Run unexplored genres tests
	$(PYTEST) tests/services/test_unexplored_genres.py -v

backend-test-now-playing: $(BACKEND_VENV_STAMP) ## Run now-playing service and route tests
	$(PYTEST) tests/services/test_now_playing.py tests/routes/test_now_playing_routes.py -v

backend-test-scrobble: $(BACKEND_VENV_STAMP) ## Run per-user scrobble service + route tests
	$(PYTEST) tests/services/test_scrobble_service.py tests/routes/test_scrobble_routes.py -v

backend-test-connections: $(BACKEND_VENV_STAMP) ## Run per-user connection stores + factory + /me routes + D10 backfill tests
	$(PYTEST) tests/infrastructure/test_user_connections_store.py tests/infrastructure/test_user_listening_prefs_store.py tests/infrastructure/test_play_history_store.py tests/services/test_per_user_client_factory.py tests/routes/test_me_connections.py tests/services/test_global_connection_backfill.py -v

backend-test-deep-discovery: $(BACKEND_VENV_STAMP) ## Run deep discovery and analytics tests
	$(PYTEST) tests/services/test_deep_discovery.py -v

backend-test-home: $(BACKEND_VENV_STAMP) ## Run home page backend tests
	$(PYTEST) tests/services/test_home_service.py tests/routes/test_home_routes.py

backend-test-profile: $(BACKEND_VENV_STAMP) ## Run per-user profile + self-service tests
	$(PYTEST) tests/routes/test_profile_routes.py tests/services/test_auth_self_service.py

backend-test-user-import: $(BACKEND_VENV_STAMP) ## Run Phase 6 admin user-import service + route tests
	$(PYTEST) tests/services/test_user_import_service.py tests/routes/test_user_import_routes.py

backend-test-home-genre: $(BACKEND_VENV_STAMP) ## Run home genre decoupling tests
	$(PYTEST) tests/services/test_home_genre_decoupling.py

backend-test-infra-hardening: $(BACKEND_VENV_STAMP) ## Run infrastructure hardening tests
	$(PYTEST) tests/infrastructure/test_circuit_breaker_sync.py tests/infrastructure/test_disk_cache_periodic.py tests/infrastructure/test_retry_non_breaking.py

backend-test-memory: $(BACKEND_VENV_STAMP) ## Run process-memory helper + playback bounding tests
	$(PYTEST) tests/infrastructure/test_memory.py tests/services/test_navidrome_playback_service.py -v

backend-test-jellyfin: $(BACKEND_VENV_STAMP) ## Run all Jellyfin integration backend tests
	$(PYTEST) tests/repositories/test_jellyfin_playback_url.py tests/services/test_jellyfin_playback_service.py tests/services/test_jellyfin_library_service.py tests/routes/test_stream_routes.py -v

backend-test-jellyfin-proxy: $(BACKEND_VENV_STAMP) ## Run Jellyfin stream proxy tests
	$(PYTEST) tests/routes/test_stream_routes.py -v

backend-test-library-pagination: $(BACKEND_VENV_STAMP) ## Run library pagination tests
	$(PYTEST) tests/infrastructure/test_library_pagination.py -v

backend-test-artists-aggregated-pagination: $(BACKEND_VENV_STAMP) ## Artists aggregation: stable pagination + folded-column search upkeep
	$(PYTEST) tests/infrastructure/test_artists_aggregated_pagination.py -v

backend-test-audio-tagger: $(BACKEND_VENV_STAMP) ## Phase 3: AudioTagger read/write roundtrip tests
	$(PYTEST) tests/services/test_audio_tagger.py -v

backend-test-naming-template-engine: $(BACKEND_VENV_STAMP) ## Phase 3: NamingTemplateEngine tests
	$(PYTEST) tests/services/test_naming_template_engine.py -v

backend-test-library-manager: $(BACKEND_VENV_STAMP) ## Phase 3: LibraryManager CRUD + concurrency tests
	$(PYTEST) tests/services/test_library_manager.py -v

backend-test-library-search-diacritics: $(BACKEND_VENV_STAMP) ## Accent-/case-insensitive library search (fold)
	$(PYTEST) tests/services/test_library_search_diacritics.py -v

backend-test-tracks-view: $(BACKEND_VENV_STAMP) ## Tracks view + multi-folder add-validation/reconcile hardening
	$(PYTEST) tests/services/test_library_manager.py tests/routes/test_library_routes.py tests/routes/test_library_settings_routes.py -v

backend-test-library-scanner: $(BACKEND_VENV_STAMP) ## Phase 4: LibraryScanner integration tests
	$(PYTEST) tests/services/test_library_scanner.py -v

backend-test-scan-scheduler: $(BACKEND_VENV_STAMP) ## Auto-scan scheduler timing (catch-up + daily-at-time)
	$(PYTEST) tests/test_auto_scan_task.py -v

backend-test-audio-fingerprinter: $(BACKEND_VENV_STAMP) ## Phase 4a: AudioFingerprinter fpcalc + AcoustID tests
	$(PYTEST) tests/services/test_audio_fingerprinter.py -v

backend-test-musicbrainz-matcher: $(BACKEND_VENV_STAMP) ## Phase 4a: MusicBrainzMatcher Tier 2/3 tests
	$(PYTEST) tests/services/test_musicbrainz_matcher.py -v

backend-test-slskd: $(BACKEND_VENV_STAMP) ## Phase 6a: SlskdClient + SlskdRepository + protocol conformance
	$(PYTEST) tests/repositories/test_slskd_client.py tests/repositories/test_slskd_repository.py tests/repositories/test_download_client_protocol_contract.py -v

backend-test-preflight-scorer: $(BACKEND_VENV_STAMP) ## Phase 6a: AlbumPreflightScorer + TrackMatcher tests
	$(PYTEST) tests/services/test_album_preflight_scorer.py tests/services/test_track_matcher.py -v

backend-test-acquisition-specs: $(BACKEND_VENV_STAMP) ## ArrRebuild: pure decision spec core (decision/context/pipeline + specs)
	$(PYTEST) tests/services/test_acquisition_specs.py -v

backend-test-download-status: $(BACKEND_VENV_STAMP) ## ArrRebuild: DownloadStatus state machine + store-CHECK mirror
	$(PYTEST) tests/services/test_download_status.py -v

backend-test-download-store: $(BACKEND_VENV_STAMP) ## Phase 6a: DownloadStore tables + CRUD + cascade tests
	$(PYTEST) tests/infrastructure/test_download_store.py -v

backend-test-follow-store: $(BACKEND_VENV_STAMP) ## Follow: FollowStore tables + CRUD + approvals + baseline detection + cascade
	$(PYTEST) tests/infrastructure/test_follow_store.py -v

backend-test-follow: $(BACKEND_VENV_STAMP) ## Follow: store + FollowService + follow/auto-download + admin approvals routes
	$(PYTEST) tests/infrastructure/test_follow_store.py tests/test_artist_monitoring.py tests/routes/test_follow_routes.py tests/routes/test_follow_approvals_routes.py -v

backend-test-new-releases: $(BACKEND_VENV_STAMP) ## Follow: NewReleaseService poll detection + fan-out + degradation
	$(PYTEST) tests/services/test_new_release_service.py -v

backend-test-download-service: $(BACKEND_VENV_STAMP) ## Phase 6b: DownloadService search/pick/cancel + mount check
	$(PYTEST) tests/services/test_download_service.py -v

backend-test-download-routes: $(BACKEND_VENV_STAMP) ## Phase 6b/7: download-client + search + quarantine + queue/task route tests
	$(PYTEST) tests/routes/test_download_client_routes.py tests/routes/test_download_search_routes.py tests/routes/test_quarantine_routes.py tests/routes/test_downloads_routes.py -v

backend-test-orchestrator: $(BACKEND_VENV_STAMP) ## Phase 7: DownloadOrchestrator + FileProcessor.process_downloaded
	$(PYTEST) tests/services/test_download_orchestrator.py tests/services/test_file_processor.py -v

backend-test-usenet: $(BACKEND_VENV_STAMP) ## Usenet/SABnzbd: protocol split, Newznab, SABnzbd, folder-import, routing, migration gates
	$(PYTEST) tests/repositories/test_download_client_protocol_contract.py \
		tests/infrastructure/test_download_migration.py \
		tests/repositories/test_newznab.py \
		tests/repositories/test_sabnzbd.py \
		tests/routes/test_indexer_routes.py \
		tests/routes/test_download_clients_routes.py \
		tests/services/test_preferences_indexers.py \
		tests/services/test_preferences_download_clients.py \
		tests/services/test_newznab_release_scorer.py \
		tests/infrastructure/test_e2e_usenet.py -v

backend-test-e2e-download: $(BACKEND_VENV_STAMP) ## Phase 7: blocking E2E gate (search -> import -> library_files)
	$(PYTEST) tests/infrastructure/test_e2e_download.py -v

security-tests: $(BACKEND_VENV_STAMP) ## Phase 9: security suite (no-secrets-in-logs, auth matrix, api-key masking)
	$(PYTEST) tests/security -v

docs-check: ## Phase 9: verify the native-engine docs exist and are non-empty
	@for f in docs/SETUP.md docs/NATIVE_ENGINE.md docs/SLSKD_SETUP.md; do \
		test -s "$(ROOT_DIR)/$$f" || { echo "docs-check FAILED: missing or empty $$f"; exit 1; }; \
	done
	@echo "docs-check OK: SETUP.md, NATIVE_ENGINE.md, SLSKD_SETUP.md present and non-empty"

e2e: $(BACKEND_VENV_STAMP) ## Phase 9: full e2e suite (mock + optional real-slskd container)
	$(PYTEST) tests/e2e tests/infrastructure/test_e2e_download.py -v

e2e-fast: $(BACKEND_VENV_STAMP) ## Phase 9: e2e suite, mock-only (skips the real-slskd container test)
	$(PYTEST) tests/e2e tests/infrastructure/test_e2e_download.py -m "not e2e" -v

backend-test-sse-publisher: $(BACKEND_VENV_STAMP) ## Phase 3b: SSEPublisher pub/sub tests
	$(PYTEST) tests/infrastructure/test_sse_publisher.py -v

backend-test-p2-post-deletion: $(BACKEND_VENV_STAMP) ## Phase 2 gate: old integration deleted, app still boots empty
	$(PYTEST) tests/repositories/test_lidarr_directory_empty.py tests/test_phase1_brownout.py -v

backend-test-local-files: $(BACKEND_VENV_STAMP) ## Run native local-files service + stream route tests
	$(PYTEST) tests/services/test_local_files_service.py tests/routes/test_stream_routes.py -v

backend-test-monitoring-cache: $(BACKEND_VENV_STAMP) ## Run artist monitoring cache/flag refresh tests
	$(PYTEST) tests/services/test_refresh_library_flags.py tests/services/test_artist_utils_tags.py -v

backend-test-multidisc: $(BACKEND_VENV_STAMP) ## Run multi-disc album tests
	$(PYTEST) tests/services/test_album_utils.py tests/services/test_album_service.py tests/infrastructure/test_cache_layer_followups.py

backend-test-navidrome: $(BACKEND_VENV_STAMP) ## Run all Navidrome integration backend tests
	$(PYTEST) tests/repositories/test_navidrome_repository.py tests/services/test_navidrome_library_service.py tests/services/test_navidrome_playback_service.py tests/services/test_navidrome_cache_invalidation.py tests/services/test_navidrome_stream_proxy.py tests/routes/test_navidrome_routes.py -v

backend-test-performance: $(BACKEND_VENV_STAMP) ## Run performance regression tests
	$(PYTEST) tests/services/test_album_singleflight.py tests/services/test_artist_singleflight.py tests/services/test_genre_batch_parallel.py tests/services/test_cache_stats_nonblocking.py tests/services/test_settings_cache_invalidation.py tests/services/test_discover_enrich_singleflight.py

backend-test-preferences: $(BACKEND_VENV_STAMP) ## Run release-type preferences persistence + consumer (artist/search) tests
	$(PYTEST) tests/services/test_preferences_generic_settings.py tests/services/test_preferences_library_settings.py tests/services/test_settings_cache_invalidation.py tests/services/test_search_service.py tests/services/test_artist_release_pagination.py tests/test_cache_key_contracts.py tests/repositories/test_musicbrainz_recording_search.py -v

backend-test-playlist: $(BACKEND_VENV_STAMP) ## Run playlist tests
	$(PYTEST) tests/services/test_playlist_service.py tests/services/test_playlist_source_resolution.py tests/repositories/test_playlist_repository.py tests/routes/test_playlist_routes.py

backend-test-playlist-ownership: $(BACKEND_VENV_STAMP) ## Run playlist ownership/redaction tests
	$(PYTEST) tests/routes/test_playlist_ownership.py

backend-test-queue-strategies: $(BACKEND_VENV_STAMP) ## Run queue strategy extraction tests
	$(PYTEST) tests/services/test_queue_strategies.py -v

backend-test-request-service: $(BACKEND_VENV_STAMP) ## Run request service tests
	$(PYTEST) tests/services/test_request_service.py -v

backend-test-search-top-result: $(BACKEND_VENV_STAMP) ## Run search top result detection tests
	$(PYTEST) tests/services/test_search_top_result.py -v

backend-test-security: $(BACKEND_VENV_STAMP) ## Run security regression tests
	$(PYTEST) tests/test_rate_limiter_middleware.py tests/test_url_validation.py tests/test_error_leakage.py

backend-test-source-playlists: $(BACKEND_VENV_STAMP) ## Run source playlist import tests (Plex, Navidrome, Jellyfin)
	$(PYTEST) tests/services/test_source_playlist_import.py -v

backend-test-content-enrichment: $(BACKEND_VENV_STAMP) ## Run content enrichment tests (lyrics, album info, audio quality)
	$(PYTEST) tests/services/test_content_enrichment.py -v

backend-test-username-login: $(BACKEND_VENV_STAMP) ## Run Phase 1 username-login + backfill tests
	$(PYTEST) tests/services/test_username_login.py tests/routes/test_auth_username.py -v

backend-test-peer-review-fixes: $(BACKEND_VENV_STAMP) ## Run peer review fix regression tests
	$(PYTEST) tests/test_peer_review_fixes.py -v

backend-test-plex: $(BACKEND_VENV_STAMP) ## Run all Plex integration backend tests
	$(PYTEST) tests/repositories/test_plex_repository.py tests/services/test_plex_playback_service.py tests/services/test_plex_library_service.py tests/routes/test_plex_routes.py tests/routes/test_plex_settings.py tests/routes/test_plex_auth.py tests/services/test_plex_integration_status.py tests/services/test_plex_settings_lifecycle.py -v

backend-test-plex-repository: $(BACKEND_VENV_STAMP) ## Run Plex repository unit tests
	$(PYTEST) tests/repositories/test_plex_repository.py -v

backend-test-plex-routes: $(BACKEND_VENV_STAMP) ## Run Plex route and settings tests
	$(PYTEST) tests/routes/test_plex_routes.py tests/routes/test_plex_settings.py tests/routes/test_plex_auth.py -v

backend-test-sync-coordinator: $(BACKEND_VENV_STAMP) ## Run sync coordinator tests (cooldown, dedup)
	$(PYTEST) tests/test_sync_coordinator.py -v

backend-test-sync-generation: $(BACKEND_VENV_STAMP) ## Run MUS-19 sync generation counter tests
	$(PYTEST) tests/test_sync_generation.py -v

backend-test-sync-resume: $(BACKEND_VENV_STAMP) ## Run sync resume-on-failure tests
	$(PYTEST) tests/test_sync_resume.py -v

backend-test-sync-watchdog: $(BACKEND_VENV_STAMP) ## Run adaptive watchdog timeout tests
	$(PYTEST) tests/test_sync_watchdog.py -v

backend-test-discover-all: backend-test-queue-strategies backend-test-daily-mix backend-test-discover-picks backend-test-discover-radio backend-test-unexplored-genres backend-test-playlist-suggestions backend-test-genre-index ## Run all discover expansion tests

test-discover-all: backend-test-discover-all frontend-test-discover-page ## Run all discover expansion tests (backend + frontend)

test-audiodb-all: backend-test-audiodb backend-test-audiodb-prewarm backend-test-audiodb-settings backend-test-coverart-audiodb backend-test-audiodb-phase8 backend-test-audiodb-phase9 frontend-test-audiodb-images ## Run every AudioDB test target

test-mus14-all: backend-test-dedup-cancellation backend-test-request-service ## Run all MUS-14 request system tests

test-sync-all: backend-test-sync-watchdog backend-test-sync-resume backend-test-audiodb-parallel backend-test-sync-generation ## Run all sync reliability tests

frontend-install: ## Install frontend npm dependencies
	cd "$(FRONTEND_DIR)" && $(NPM) install

frontend-build: ## Run frontend production build
	cd "$(FRONTEND_DIR)" && $(NPM) run build

frontend-browser-install: ## Install Playwright Chromium for browser tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec playwright install chromium

frontend-format-check: ## Run frontend formatting checks
	cd "$(FRONTEND_DIR)" && $(NPM) run format:check

frontend-check: ## Run frontend type checks
	cd "$(FRONTEND_DIR)" && $(NPM) run check

frontend-lint: ## Run frontend linting
	cd "$(FRONTEND_DIR)" && $(NPM) run lint

frontend-test: ## Run the frontend vitest suite (all projects, needs Playwright)
	cd "$(FRONTEND_DIR)" && $(NPM) run test

frontend-test-server: ## Run frontend server-project tests only (no Playwright)
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server

frontend-test-client: ## Run frontend client-project tests only (chromium, needs Playwright)
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project client

frontend-test-connections: ## Run per-user connections + scrobble-preferences frontend tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/queries/connections src/lib/queries/scrobble-preferences

frontend-test-home-discover: ## Run Phase 5 per-user home/discover key + cache-isolation frontend tests (AMU-5/AMU-8)
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run src/lib/queries/HomeQueryKeyFactory.spec.ts src/lib/queries/discover/DiscoverQueryKeyFactory.spec.ts src/lib/queries/discover/DiscoverQuery.spec.ts src/lib/queries/clearOnUserSwitch.svelte.spec.ts src/lib/utils/discoverQueueCache.svelte.spec.ts src/lib/components/TimeRangeView.svelte.spec.ts src/lib/components/RadioSection.spec.ts

frontend-test-e2e: ## Phase 9: Playwright download-flow E2E (needs a running stack; not in ci)
	cd "$(FRONTEND_DIR)" && $(NPM) exec playwright test

frontend-test-integration-coverage: ## Phase 9: assert every native route has a frontend API surface
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/queries/__tests__/integration-coverage.spec.ts

frontend-test-downloads-ui: ## Phase 6b: download-client + search component tests (chromium)
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project client src/lib/components/settings/SettingsDownloadClient.svelte.spec.ts src/lib/components/downloads/SearchResultCard.svelte.spec.ts

frontend-test-album-page: ## Run the album page browser test
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project client src/routes/album/[id]/page.svelte.spec.ts

frontend-test-audiodb-images: ## Run AudioDB image tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/utils/imageSuffix.spec.ts
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project client src/lib/components/BaseImage.svelte.spec.ts

frontend-test-follow: ## Run Follow feature query/mutation data-layer tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/queries/following

frontend-test-playlist-detail: ## Run playlist page browser tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project client src/routes/playlists/[id]/page.svelte.spec.ts

frontend-test-queuehelpers: ## Run queue helper regressions
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/player/queueHelpers.spec.ts

frontend-test-plex: ## Run Plex frontend tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/player/plexPlaybackApi.spec.ts src/lib/player/launchPlexPlayback.spec.ts

frontend-test-navidrome: ## Run Navidrome frontend tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/player/queueHelpers.spec.ts

frontend-test-jellyfin: ## Run Jellyfin frontend tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/player/jellyfinPlaybackApi.spec.ts

frontend-test-discover-page: ## Run discover page and query tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/queries/discover/DiscoverQuery.spec.ts

frontend-test-auth: ## Run auth query/mutation data-layer tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/queries/auth/AuthMutations.spec.ts

frontend-test-user-import: ## Run Phase 6 admin user-import query/mutation data-layer tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/queries/auth/UserImportMutations.spec.ts

frontend-test-profile: ## Run profile query/mutation + cache-isolation tests
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run --project server src/lib/queries/profile/ProfileMutations.spec.ts

frontend-test-auth-username: ## Run Phase 1 auth username frontend tests (data layer + login page)
	cd "$(FRONTEND_DIR)" && $(NPM) exec vitest run src/lib/queries/auth src/routes/login

rebuild: ## Rebuild the application
	cd "$(ROOT_DIR)" && ./manage.sh --rebuild

fmt: format ## Alias for 'format'

format: ## Auto-format backend (ruff --fix) and frontend (prettier)
	cd "$(ROOT_DIR)" && $(BACKEND_VENV_DIR)/bin/ruff check --fix backend
	cd "$(FRONTEND_DIR)" && $(NPM) run format

lint: backend-lint frontend-lint ## Run all linting checks

tests: backend-test frontend-test-server ## Run all tests
test: tests ## Alias for 'tests'

ci: backend-lint frontend-lint frontend-check frontend-format-check backend-test frontend-test-server security-tests e2e-fast docs-check ## Run the full CI pipeline (lint + typecheck + tests + security + e2e + docs)
