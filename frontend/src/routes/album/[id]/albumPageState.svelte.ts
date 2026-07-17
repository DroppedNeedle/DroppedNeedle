import { browser } from '$app/environment';
import { get } from 'svelte/store';
import { untrack } from 'svelte';
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import type {
	AlbumBasicInfo,
	AlbumTracksInfo,
	MoreByArtistResponse,
	SimilarAlbumsResponse,
	YouTubeTrackLink,
	YouTubeLink,
	YouTubeQuotaStatus,
	JellyfinAlbumMatch,
	JellyfinTrackInfo,
	LocalAlbumMatch,
	LocalTrackInfo,
	NavidromeAlbumMatch,
	NavidromeTrackInfo,
	PlexAlbumMatch,
	PlexTrackInfo,
	LastFmAlbumEnrichment,
	LibraryAlbumStatus,
	LibraryFileMeta,
	DownloadTask,
	HeldImport
} from '$lib/types';
import { libraryStore } from '$lib/stores/library';
import { integrationStore } from '$lib/stores/integration';
import { API } from '$lib/constants';
import { isAbortError } from '$lib/utils/errorHandling';
import { extractServiceStatus } from '$lib/utils/serviceStatus';
import {
	albumBasicCache,
	albumDiscoveryCache,
	albumLastFmCache,
	albumTracksCache,
	albumYouTubeCache,
	albumSourceMatchCache
} from '$lib/utils/albumDetailCache';
import { hydrateDetailCacheEntry } from '$lib/utils/detailCacheHydration';
import { compareDiscTrack, getDiscTrackKey } from '$lib/player/queueHelpers';
import type { QueueItem } from '$lib/player/types';
import { launchJellyfinPlayback } from '$lib/player/launchJellyfinPlayback';
import { launchLocalPlayback } from '$lib/player/launchLocalPlayback';
import { launchNavidromePlayback } from '$lib/player/launchNavidromePlayback';
import { launchPlexPlayback } from '$lib/player/launchPlexPlayback';
import { downloadFile } from '$lib/utils/downloadHelper';
import type { MenuItem } from '$lib/components/ContextMenu.svelte';
import {
	fetchAlbumBasic,
	fetchAlbumTracks,
	fetchDiscovery,
	fetchYouTubeAlbumLink,
	fetchYouTubeTrackLinks,
	fetchJellyfinMatch,
	fetchLocalMatch,
	fetchNavidromeMatch,
	fetchPlexMatch,
	fetchLastFm,
	refreshAlbum
} from './albumFetchers';
import { buildRenderedTrackSections, buildSortedTrackMap } from './albumTrackResolvers';
import type { RenderedTrackSection } from './albumTrackResolvers';
import { createEventHandlers } from './albumEventHandlers';
import {
	playSourceTrack as playSourceTrackImpl,
	getTrackContextMenuItems as getTrackContextMenuItemsImpl,
	buildSourceCallbacks
} from './albumPlaybackHandlers';
import { getLibraryAlbumStatusQuery } from '$lib/queries/library/LibraryQueries.svelte';
import { getAlbumDownloadsQuery } from '$lib/queries/downloads/DownloadQueries.svelte';
import { getHeldImportsQuery } from '$lib/queries/downloads/HeldQueries.svelte';
import { isActiveDownloadStatus } from '$lib/queries/downloads/downloadStatus';
import { authStore } from '$lib/stores/authStore.svelte';
import { getNavidromeFolderScopeRevision } from '$lib/utils/navidromeLibraryCache';

export interface SourceCallbacks {
	onPlayAll: () => void;
	onShuffle: () => void;
	onAddAllToQueue: () => void;
	onPlayAllNext: () => void;
	onAddAllToPlaylist: () => void;
}

export function createAlbumPageState(albumIdGetter: () => string) {
	const sourceCacheKey = (albumId: string) =>
		[
			authStore.user?.id ?? 'anonymous',
			getNavidromeFolderScopeRevision(authStore.user?.id ?? ''),
			albumId
		]
			.map(encodeURIComponent)
			.join(':');
	let album = $state<AlbumBasicInfo | null>(null);
	let tracksInfo = $state<AlbumTracksInfo | null>(null);
	let error = $state<string | null>(null);
	let loadingBasic = $state(true);
	let loadingTracks = $state(true);
	let tracksError = $state(false);
	let showToast = $state(false);
	let toastMessage = $state('Added to Library');
	let toastType = $state<'success' | 'error' | 'info' | 'warning'>('success');
	let requesting = $state(false);
	let showDeleteModal = $state(false);
	let moreByArtist = $state<MoreByArtistResponse | null>(null);
	let similarAlbums = $state<SimilarAlbumsResponse | null>(null);
	let loadingDiscovery = $state(true);
	let trackLinks = $state<YouTubeTrackLink[]>([]);
	let albumLink = $state<YouTubeLink | null>(null);
	let quota = $state<YouTubeQuotaStatus | null>(null);
	let jellyfinMatch = $state<JellyfinAlbumMatch | null>(null);
	let localMatch = $state<LocalAlbumMatch | null>(null);
	let navidromeMatch = $state<NavidromeAlbumMatch | null>(null);
	let plexMatch = $state<PlexAlbumMatch | null>(null);
	let loadingJellyfin = $state(false);
	let loadingLocal = $state(false);
	let loadingNavidrome = $state(false);
	let loadingPlex = $state(false);
	let lastfmEnrichment = $state<LastFmAlbumEnrichment | null>(null);
	let loadingLastfm = $state(true);
	let renderedTrackSections = $state<RenderedTrackSection[]>([]);
	let playlistModalRef = $state<{ open: (tracks: QueueItem[]) => void } | null>(null);
	let abortController: AbortController | null = null;
	let refreshing = $state(false);
	let downloadClientConfigured = $state(false);
	let externalRecheckTimers: ReturnType<typeof setTimeout>[] = [];
	// task ids we've watched go active this session, and the ones whose completion we've already
	// handled - so an already-finished download present on first load doesn't trigger a spurious refresh
	const seenActiveTaskIds = new SvelteSet<string>();
	const settledTaskIds = new SvelteSet<string>();
	// shared TanStack query so tag-edit/rescan mutations that invalidate album(mbid) refresh this overlay in place
	const libraryStatusQuery = getLibraryAlbumStatusQuery(albumIdGetter);
	const libraryStatus = $derived<LibraryAlbumStatus | null>(libraryStatusQuery.data ?? null);

	const downloadsQuery = getAlbumDownloadsQuery(albumIdGetter, () => downloadClientConfigured);
	const albumDownloadTasks = $derived(downloadsQuery.data?.items ?? []);
	// what the header strip shows: any in-flight task (prefer a live downloading/processing one),
	// else the most recent attempt only if it's actionable (failed/partial -> retry). completed and
	// cancelled show nothing - the In-Library badge takes over and we don't nag on old cancellations.
	const headerDownloadTask = $derived.by<DownloadTask | null>(() => {
		if (albumDownloadTasks.length === 0) return null;
		const inflight = albumDownloadTasks.filter((t) => isActiveDownloadStatus(t.status));
		if (inflight.length > 0) {
			const live = inflight.filter((t) => t.status === 'downloading' || t.status === 'processing');
			const pool = live.length > 0 ? live : inflight;
			return pool.reduce((best, t) => (t.created_at > best.created_at ? t : best), pool[0]);
		}
		const latest = albumDownloadTasks.reduce((best, t) =>
			t.created_at > best.created_at ? t : best
		);
		return latest.status === 'failed' || latest.status === 'partial' ? latest : null;
	});
	// per-recording active task for the track rows (most recent active wins)
	const trackDownloadTasks = $derived.by(() => {
		const m = new SvelteMap<string, DownloadTask>();
		for (const t of albumDownloadTasks) {
			if (!t.recording_mbid || !isActiveDownloadStatus(t.status)) continue;
			const existing = m.get(t.recording_mbid);
			if (!existing || t.created_at > existing.created_at) m.set(t.recording_mbid, t);
		}
		return m;
	});

	$effect(() => {
		const unsub = integrationStore.subscribe((s) => (downloadClientConfigured = s.download_client));
		return unsub;
	});

	// when a task we watched go active later settles (completed/partial), refresh library status +
	// source matches once so the In-Library count and play bars update without a manual refresh
	$effect(() => {
		const tasks = albumDownloadTasks;
		untrack(() => {
			for (const t of tasks) {
				if (isActiveDownloadStatus(t.status)) {
					seenActiveTaskIds.add(t.id);
				} else if (
					(t.status === 'completed' || t.status === 'partial') &&
					seenActiveTaskIds.has(t.id) &&
					!settledTaskIds.has(t.id)
				) {
					settledTaskIds.add(t.id);
					// a fully-completed album flips to In-Library instantly (cover/card
					// badges read libraryStore, not TanStack); a partial leaves it to the
					// refetch so missing-track rows still render
					if (t.status === 'completed' && album?.musicbrainz_id) {
						libraryStore.addMbid(album.musicbrainz_id);
					}
					onDownloadSettled();
				}
			}
		});
	});

	const trackLinkMap = $derived.by(
		() => new SvelteMap(trackLinks.map((tl) => [getDiscTrackKey(tl), tl]))
	);
	const jellyfinTracks = $derived([...(jellyfinMatch?.tracks ?? [])].sort(compareDiscTrack));
	const localTracks = $derived([...(localMatch?.tracks ?? [])].sort(compareDiscTrack));
	const navidromeTracks = $derived([...(navidromeMatch?.tracks ?? [])].sort(compareDiscTrack));
	const plexTracks = $derived([...(plexMatch?.tracks ?? [])].sort(compareDiscTrack));
	const jellyfinTrackMap = $derived(buildSortedTrackMap(jellyfinMatch?.tracks ?? []));
	const localTrackMap = $derived(buildSortedTrackMap(localMatch?.tracks ?? []));
	const navidromeTrackMap = $derived(buildSortedTrackMap(navidromeMatch?.tracks ?? []));
	const plexTrackMap = $derived(buildSortedTrackMap(plexMatch?.tracks ?? []));
	const inLibrary = $derived(
		libraryStatus?.in_library ??
			(libraryStore.isInLibrary(album?.musicbrainz_id) || album?.in_library || false)
	);
	const isRequested = $derived(
		!!(album && !inLibrary && (album.requested || libraryStore.isRequested(album.musicbrainz_id)))
	);
	const libraryTracksByRecording = $derived.by(() => {
		const m = new SvelteMap<string, LibraryFileMeta>();
		for (const t of libraryStatus?.tracks ?? []) {
			if (t.musicbrainz_recording_id) m.set(t.musicbrainz_recording_id, t);
		}
		return m;
	});
	// positional fallback (disc:track): downloaded files often lack a recording MBID, and the same song has different recording MBIDs across releases, so MBID-only matching misses in-library tracks
	const libraryTracksByPosition = $derived.by(() => {
		const m = new SvelteMap<string, LibraryFileMeta>();
		for (const t of libraryStatus?.tracks ?? []) {
			m.set(getDiscTrackKey({ disc_number: t.disc_number, track_number: t.track_number }), t);
		}
		return m;
	});
	// tracks this album downloaded but couldn't auto-verify (held for "import anyway"),
	// keyed both ways so a track row can find its held candidate the same way owned files match
	const heldImportsQuery = getHeldImportsQuery(albumIdGetter, () => downloadClientConfigured);
	const heldByRecording = $derived.by(() => {
		const m = new SvelteMap<string, HeldImport>();
		for (const h of heldImportsQuery.data?.items ?? []) {
			if (h.recording_mbid) m.set(h.recording_mbid, h);
		}
		return m;
	});
	const heldByPosition = $derived.by(() => {
		const m = new SvelteMap<string, HeldImport>();
		for (const h of heldImportsQuery.data?.items ?? []) {
			m.set(getDiscTrackKey({ disc_number: h.disc_number, track_number: h.track_number }), h);
		}
		return m;
	});
	// A held track resolving ("import anyway" / discard) shrinks the held set. Refetch
	// library status + source matches so a newly-owned track's play button appears in place,
	// without a manual refresh (mirrors the settled-download effect above).
	let lastHeldItemCount = -1;
	$effect(() => {
		const count = heldImportsQuery.data?.items?.length ?? 0;
		untrack(() => {
			if (lastHeldItemCount !== -1 && count < lastHeldItemCount) {
				void libraryStatusQuery.refetch();
				refreshSourcesAfterDownload();
			}
			lastHeldItemCount = count;
		});
	});

	const libraryInLibrary = $derived(libraryStatus?.in_library ?? false);
	const libraryTrackCount = $derived(libraryStatus?.track_count ?? 0);
	// any held track below the quality cutoff -> the header's curator "Upgrade quality" affordance
	const libraryBelowCutoff = $derived((libraryStatus?.tracks ?? []).some((t) => t.below_cutoff));
	// P5 coverage: what the held files actually COVER of the release's tracklist.
	// expectedTracks === 0 -> tracklist unavailable, badge/Play All fall back to the
	// presence-only reading (never block playback on a MusicBrainz hiccup).
	const coverageExpected = $derived(libraryStatus?.expected_tracks ?? 0);
	const coverageCovered = $derived(libraryStatus?.covered_tracks ?? 0);
	const matchedFileIds = $derived(new SvelteSet(libraryStatus?.matched_file_ids ?? []));
	const libraryOrphans = $derived(libraryStatus?.orphans ?? []);

	function resetState() {
		if (abortController) {
			abortController.abort();
			abortController = null;
		}
		clearExternalRecheck();
		seenActiveTaskIds.clear();
		settledTaskIds.clear();
		lastHeldItemCount = -1;
		album = null;
		tracksInfo = null;
		renderedTrackSections = [];
		error = null;
		loadingBasic = true;
		loadingTracks = true;
		tracksError = false;
		loadingDiscovery = true;
		moreByArtist = null;
		similarAlbums = null;
		trackLinks = [];
		albumLink = null;
		quota = null;
		jellyfinMatch = null;
		localMatch = null;
		navidromeMatch = null;
		plexMatch = null;
		loadingJellyfin = false;
		loadingLocal = false;
		loadingNavidrome = false;
		loadingPlex = false;
		lastfmEnrichment = null;
		loadingLastfm = true;
		refreshing = false;
	}

	function hydrateFromCache(albumId: string) {
		const refreshBasic = hydrateDetailCacheEntry({
			cache: albumBasicCache,
			cacheKey: albumId,
			onHydrate: (cached) => {
				album = cached;
				loadingBasic = false;
			}
		});
		const refreshTracks = hydrateDetailCacheEntry({
			cache: albumTracksCache,
			cacheKey: albumId,
			onHydrate: (cached) => {
				tracksInfo = cached;
				renderedTrackSections = buildRenderedTrackSections(cached.tracks);
				loadingTracks = false;
			}
		});
		const refreshDiscovery = hydrateDetailCacheEntry({
			cache: albumDiscoveryCache,
			cacheKey: albumId,
			onHydrate: (cached) => {
				moreByArtist = cached.moreByArtist;
				similarAlbums = cached.similarAlbums;
				loadingDiscovery = false;
			}
		});
		const refreshLastfm = hydrateDetailCacheEntry({
			cache: albumLastFmCache,
			cacheKey: albumId,
			onHydrate: (cached) => {
				lastfmEnrichment = cached;
				loadingLastfm = false;
			}
		});
		const refreshSourceMatch = (() => {
			const cached = albumSourceMatchCache.get(sourceCacheKey(albumId));
			if (cached && !albumSourceMatchCache.isStale(cached.timestamp)) {
				jellyfinMatch = cached.data.jellyfin;
				localMatch = cached.data.local;
				navidromeMatch = cached.data.navidrome;
				plexMatch = cached.data.plex;
				loadingJellyfin = false;
				loadingLocal = false;
				loadingNavidrome = false;
				loadingPlex = false;
				return false;
			}
			return true;
		})();
		return { refreshBasic, refreshTracks, refreshDiscovery, refreshLastfm, refreshSourceMatch };
	}

	async function doFetchBasic(albumId: string, signal: AbortSignal) {
		try {
			const result = await fetchAlbumBasic(albumId, signal);
			if (result) {
				album = result;
				extractServiceStatus(album);
				albumBasicCache.set(album, albumId);
			}
		} catch (e) {
			if (isAbortError(e)) return;
			if (!album) error = 'Error loading album';
		} finally {
			if (!signal.aborted) loadingBasic = false;
		}
	}

	async function doFetchTracks(albumId: string, signal: AbortSignal) {
		tracksError = false;
		try {
			const result = await fetchAlbumTracks(albumId, signal);
			if (result) {
				tracksInfo = result;
				renderedTrackSections = buildRenderedTrackSections(result.tracks);
				albumTracksCache.set(result, albumId);
			}
		} catch (e) {
			if (isAbortError(e)) return;
			if (!tracksInfo) tracksError = true;
		}
		if (!signal.aborted) loadingTracks = false;
	}

	async function doFetchDiscovery(albumId: string, signal: AbortSignal) {
		if (!album?.artist_id) {
			loadingDiscovery = false;
			return;
		}
		loadingDiscovery = true;
		try {
			const result = await fetchDiscovery(albumId, album.artist_id, signal);
			if (result.moreByArtist) moreByArtist = result.moreByArtist;
			if (result.similarAlbums) similarAlbums = result.similarAlbums;
			albumDiscoveryCache.set({ moreByArtist, similarAlbums }, albumId);
		} catch (e) {
			if (isAbortError(e)) return;
		} finally {
			if (!signal.aborted) loadingDiscovery = false;
		}
	}

	async function doFetchYouTube(albumId: string, signal: AbortSignal) {
		const cached = albumYouTubeCache.get(albumId);
		if (cached && !albumYouTubeCache.isStale(cached.timestamp)) {
			albumLink = cached.data.albumLink;
			trackLinks = cached.data.trackLinks;
			return;
		}
		try {
			const [linkData, tracksData] = await Promise.all([
				fetchYouTubeAlbumLink(albumId, signal),
				fetchYouTubeTrackLinks(albumId, signal)
			]);
			if (linkData) albumLink = linkData;
			if (tracksData) trackLinks = tracksData;
			albumYouTubeCache.set({ albumLink: linkData, trackLinks: tracksData ?? [] }, albumId);
		} catch (e) {
			if (isAbortError(e)) return;
		}
	}

	async function doFetchSourceMatch<T>(
		signal: AbortSignal,
		fetcher: () => Promise<T | null>,
		setter: (v: T | null) => void,
		loadingSetter: (v: boolean) => void,
		label: string,
		albumId: string,
		cacheField: 'jellyfin' | 'local' | 'navidrome' | 'plex'
	) {
		loadingSetter(true);
		try {
			const result = await fetcher();
			setter(result);
			const cacheKey = sourceCacheKey(albumId);
			const existing = albumSourceMatchCache.get(cacheKey)?.data ?? {
				jellyfin: null,
				local: null,
				navidrome: null,
				plex: null
			};
			albumSourceMatchCache.set({ ...existing, [cacheField]: result }, cacheKey);
		} catch (e) {
			if (isAbortError(e)) return;
		} finally {
			if (!signal.aborted) loadingSetter(false);
		}
	}

	async function doFetchLastFm(albumId: string, signal: AbortSignal) {
		if (!album) {
			loadingLastfm = false;
			return;
		}
		// global/shared feature; backend returns null when Last.fm isn't configured
		loadingLastfm = true;
		try {
			const result = await fetchLastFm(
				albumId,
				{ artistName: album.artist_name, albumName: album.title },
				signal
			);
			if (result) {
				lastfmEnrichment = result;
				albumLastFmCache.set(result, albumId);
			}
		} catch (e) {
			if (isAbortError(e)) return;
		} finally {
			if (!signal.aborted) loadingLastfm = false;
		}
	}

	// MBID-only source matches (Jellyfin, Local Files) - can start before basic info loads
	async function fetchMbidSourceMatches(albumId: string, signal: AbortSignal) {
		try {
			await integrationStore.ensureLoaded();
			if (signal.aborted) return;
			const integrations = get(integrationStore);
			if (integrations.jellyfin)
				void doFetchSourceMatch(
					signal,
					() => fetchJellyfinMatch(albumId, signal),
					(v) => (jellyfinMatch = v),
					(v) => (loadingJellyfin = v),
					'Jellyfin',
					albumId,
					'jellyfin'
				);
			if (integrations.localfiles)
				void doFetchSourceMatch(
					signal,
					() => fetchLocalMatch(albumId, signal),
					(v) => (localMatch = v),
					(v) => (loadingLocal = v),
					'local',
					albumId,
					'local'
				);
		} catch {
			return;
		}
	}

	// name-based source matches (Navidrome, Plex) - need album title/artist from basic info
	async function fetchNamedSourceMatches(albumId: string, signal: AbortSignal) {
		try {
			await integrationStore.ensureLoaded();
			if (signal.aborted) return;
			const integrations = get(integrationStore);
			if (integrations.navidrome)
				void doFetchSourceMatch(
					signal,
					() =>
						fetchNavidromeMatch(
							albumId,
							{ albumTitle: album?.title, artistName: album?.artist_name },
							signal
						),
					(v) => (navidromeMatch = v),
					(v) => (loadingNavidrome = v),
					'Navidrome',
					albumId,
					'navidrome'
				);
			if (integrations.plex)
				void doFetchSourceMatch(
					signal,
					() =>
						fetchPlexMatch(
							albumId,
							{ albumTitle: album?.title, artistName: album?.artist_name },
							signal
						),
					(v) => (plexMatch = v),
					(v) => (loadingPlex = v),
					'Plex',
					albumId,
					'plex'
				);
		} catch {
			return;
		}
	}

	async function loadAlbum(albumId: string) {
		const { refreshBasic, refreshTracks, refreshDiscovery, refreshLastfm, refreshSourceMatch } =
			hydrateFromCache(albumId);
		if (abortController) abortController.abort();
		abortController = new AbortController();
		const signal = abortController.signal;
		let tracksPromise: Promise<void> | null = null;

		if (refreshSourceMatch) void fetchMbidSourceMatches(albumId, signal);

		if (refreshBasic) {
			if (refreshTracks) tracksPromise = doFetchTracks(albumId, signal);
			void doFetchYouTube(albumId, signal);
			await doFetchBasic(albumId, signal);
		} else {
			void doFetchBasic(albumId, signal);
		}
		if (signal.aborted || !album) return;
		if (refreshTracks && !refreshBasic) tracksPromise = doFetchTracks(albumId, signal);
		if (!refreshBasic) void doFetchYouTube(albumId, signal);
		if (refreshLastfm) void doFetchLastFm(albumId, signal);
		if (refreshSourceMatch) void fetchNamedSourceMatches(albumId, signal);
		if (refreshDiscovery) {
			if (tracksPromise) {
				void tracksPromise.then(() => {
					if (!signal.aborted) void doFetchDiscovery(albumId, signal);
				});
			} else {
				void doFetchDiscovery(albumId, signal);
			}
		}
	}

	function clearExternalRecheck() {
		for (const t of externalRecheckTimers) clearTimeout(t);
		externalRecheckTimers = [];
	}

	function refreshSourcesAfterDownload(): void {
		const albumId = albumIdGetter();
		const signal = abortController?.signal;
		if (!albumId || !signal || signal.aborted) return;
		albumSourceMatchCache.remove(sourceCacheKey(albumId));
		void fetchMbidSourceMatches(albumId, signal);
		void fetchNamedSourceMatches(albumId, signal);
	}

	// external servers (Jellyfin/Navidrome/Plex) index newly-imported files on their own schedule, so
	// re-check a couple of times after a download lands, then stop. Local Files is the native library
	// and is already fresh from the immediate refresh.
	function scheduleExternalSourceRecheck(): void {
		clearExternalRecheck();
		const integrations = get(integrationStore);
		if (!integrations.jellyfin && !integrations.navidrome && !integrations.plex) return;
		for (const delay of [20_000, 50_000]) {
			externalRecheckTimers.push(setTimeout(() => refreshSourcesAfterDownload(), delay));
		}
	}

	function onDownloadSettled(): void {
		void libraryStatusQuery.refetch();
		refreshSourcesAfterDownload();
		scheduleExternalSourceRecheck();
	}

	async function forceLoadAlbum(albumId: string): Promise<void> {
		albumBasicCache.remove(albumId);
		albumTracksCache.remove(albumId);
		albumSourceMatchCache.remove(sourceCacheKey(albumId));

		if (abortController) abortController.abort();
		abortController = new AbortController();
		const signal = abortController.signal;

		try {
			const freshBasic = await refreshAlbum(albumId, signal);
			if (freshBasic) {
				album = freshBasic;
				extractServiceStatus(album);
				albumBasicCache.set(album, albumId);
			}
		} catch {
			void signal.aborted;
		}

		if (signal.aborted) return;
		await loadAlbum(albumId);
	}

	async function refreshAll(): Promise<void> {
		const albumId = albumIdGetter();
		if (!albumId || refreshing) return;
		refreshing = true;
		try {
			void libraryStatusQuery.refetch();
			await forceLoadAlbum(albumId);
		} finally {
			refreshing = false;
		}
	}

	function handleDeleted(): void {
		showDeleteModal = false;
		if (album) {
			album = { ...album, in_library: false, requested: false };
			albumBasicCache.set(album, albumIdGetter());
		}
		localMatch = null;
		albumSourceMatchCache.remove(sourceCacheKey(albumIdGetter()));
		toastMessage = 'Removed from Library';
		toastType = 'success';
		showToast = true;
	}

	$effect(() => {
		const albumId = albumIdGetter();
		if (!browser || !albumId) return;
		untrack(() => {
			resetState();
			void loadAlbum(albumId);
		});
		return () => {
			clearExternalRecheck();
			if (abortController) {
				abortController.abort();
				abortController = null;
			}
		};
	});

	const eventHandlers = createEventHandlers({
		getAlbum: () => album,
		setAlbum: (a) => (album = a),
		getAlbumId: albumIdGetter,
		albumBasicCacheSet: (data, key) => albumBasicCache.set(data, key),
		setTrackLinks: (tl) => (trackLinks = tl),
		getTrackLinks: () => trackLinks,
		setAlbumLink: (l) => (albumLink = l),
		setQuota: (q) => (quota = q),
		setRequesting: (v) => (requesting = v),
		getRequesting: () => requesting,
		setShowDeleteModal: (v) => (showDeleteModal = v),
		setToast: (msg, type) => {
			toastMessage = msg;
			toastType = type;
		},
		setShowToast: (v) => (showToast = v),
		onRequestSuccess: () => {
			albumSourceMatchCache.remove(sourceCacheKey(albumIdGetter()));
			// pick up the freshly-created download task so the header strip + polling kick in
			void downloadsQuery.refetch();
		}
	});

	function retryTracks(): void {
		loadingTracks = true;
		tracksError = false;
		const signal = abortController?.signal ?? new AbortController().signal;
		void doFetchTracks(albumIdGetter(), signal);
	}

	// Playback sees only files that COVER an expected track (P5, 2026-07-05
	// incident): an orphan ("doesn't match this album") never enters the album
	// queue - it plays individually from the review section. Falls back to the
	// raw match when the tracklist was unavailable (expected_tracks === 0).
	const localMatchForPlayback = $derived.by(() => {
		if (!localMatch || coverageExpected === 0) return localMatch;
		return {
			...localMatch,
			tracks: localMatch.tracks.filter((t) => matchedFileIds.has(String(t.track_file_id)))
		};
	});
	const localTracksForPlayback = $derived(localMatchForPlayback?.tracks ?? []);

	const tracksGetters = {
		jellyfin: () => jellyfinTracks,
		local: () => localTracksForPlayback,
		navidrome: () => navidromeTracks,
		plex: () => plexTracks
	};
	const albumGetter = () => album;
	const playlistRefGetter = () => playlistModalRef;

	function playSourceTrack(
		source: 'jellyfin' | 'local' | 'navidrome' | 'plex',
		trackPosition: number,
		discNumber: number,
		title: string
	): void {
		if (!album) return;
		playSourceTrackImpl(
			source,
			trackPosition,
			discNumber,
			title,
			album,
			jellyfinMatch,
			localMatch,
			navidromeMatch,
			plexMatch
		);
	}

	function getTrackContextMenuItems(
		track: { position: number; disc_number?: number | null; title: string },
		resolvedLocal: LocalTrackInfo | null,
		resolvedJellyfin: JellyfinTrackInfo | null,
		resolvedNavidrome: NavidromeTrackInfo | null = null,
		resolvedPlex: PlexTrackInfo | null = null
	): MenuItem[] {
		if (!album) return [];
		return getTrackContextMenuItemsImpl(
			track,
			album,
			resolvedLocal,
			resolvedJellyfin,
			resolvedNavidrome,
			resolvedPlex,
			playlistModalRef
		);
	}

	const localDownloadCallback = $derived<{ callback: (() => void) | undefined }>(
		(() => {
			const mbid = localMatch?.musicbrainz_id;
			return {
				callback: mbid ? () => downloadFile(API.download.localAlbumByMbid(mbid)) : undefined
			};
		})()
	);

	const jellyfinCallbacks: SourceCallbacks = buildSourceCallbacks(
		() => jellyfinMatch,
		launchJellyfinPlayback,
		'jellyfin',
		albumGetter,
		tracksGetters,
		playlistRefGetter
	);
	const localCallbacks: SourceCallbacks = buildSourceCallbacks(
		() => localMatchForPlayback,
		launchLocalPlayback,
		'local',
		albumGetter,
		tracksGetters,
		playlistRefGetter
	);
	const navidromeCallbacks: SourceCallbacks = buildSourceCallbacks(
		() => navidromeMatch,
		launchNavidromePlayback,
		'navidrome',
		albumGetter,
		tracksGetters,
		playlistRefGetter
	);
	const plexCallbacks: SourceCallbacks = buildSourceCallbacks(
		() => plexMatch,
		launchPlexPlayback,
		'plex',
		albumGetter,
		tracksGetters,
		playlistRefGetter
	);

	return {
		get album() {
			return album;
		},
		get tracksInfo() {
			return tracksInfo;
		},
		get error() {
			return error;
		},
		get loadingBasic() {
			return loadingBasic;
		},
		get loadingTracks() {
			return loadingTracks;
		},
		get tracksError() {
			return tracksError;
		},
		get showToast() {
			return showToast;
		},
		set showToast(v: boolean) {
			showToast = v;
		},
		get toastMessage() {
			return toastMessage;
		},
		get toastType() {
			return toastType;
		},
		get requesting() {
			return requesting;
		},
		get showDeleteModal() {
			return showDeleteModal;
		},
		set showDeleteModal(v: boolean) {
			showDeleteModal = v;
		},
		get moreByArtist() {
			return moreByArtist;
		},
		get similarAlbums() {
			return similarAlbums;
		},
		get loadingDiscovery() {
			return loadingDiscovery;
		},
		get trackLinks() {
			return trackLinks;
		},
		get albumLink() {
			return albumLink;
		},
		get quota() {
			return quota;
		},
		get jellyfinMatch() {
			return jellyfinMatch;
		},
		get localMatch() {
			return localMatch;
		},
		get navidromeMatch() {
			return navidromeMatch;
		},
		get plexMatch() {
			return plexMatch;
		},
		get loadingJellyfin() {
			return loadingJellyfin;
		},
		get loadingLocal() {
			return loadingLocal;
		},
		get loadingNavidrome() {
			return loadingNavidrome;
		},
		get loadingPlex() {
			return loadingPlex;
		},
		get lastfmEnrichment() {
			return lastfmEnrichment;
		},
		get loadingLastfm() {
			return loadingLastfm;
		},
		get renderedTrackSections() {
			return renderedTrackSections;
		},
		get trackLinkMap() {
			return trackLinkMap;
		},
		get jellyfinTracks() {
			return jellyfinTracks;
		},
		get localTracks() {
			return localTracks;
		},
		get navidromeTracks() {
			return navidromeTracks;
		},
		get plexTracks() {
			return plexTracks;
		},
		get jellyfinTrackMap() {
			return jellyfinTrackMap;
		},
		get localTrackMap() {
			return localTrackMap;
		},
		get navidromeTrackMap() {
			return navidromeTrackMap;
		},
		get plexTrackMap() {
			return plexTrackMap;
		},
		get inLibrary() {
			return inLibrary;
		},
		get isRequested() {
			return isRequested;
		},
		get libraryStatus() {
			return libraryStatus;
		},
		get libraryTracksByRecording() {
			return libraryTracksByRecording;
		},
		get libraryTracksByPosition() {
			return libraryTracksByPosition;
		},
		get heldByRecording() {
			return heldByRecording;
		},
		get heldByPosition() {
			return heldByPosition;
		},
		get libraryInLibrary() {
			return libraryInLibrary;
		},
		get libraryTrackCount() {
			return libraryTrackCount;
		},
		get libraryBelowCutoff() {
			return libraryBelowCutoff;
		},
		get coverageExpected() {
			return coverageExpected;
		},
		get localMatchForPlayback() {
			return localMatchForPlayback;
		},
		get coverageCovered() {
			return coverageCovered;
		},
		get libraryOrphans() {
			return libraryOrphans;
		},
		get refreshing() {
			return refreshing;
		},
		get headerDownloadTask() {
			return headerDownloadTask;
		},
		get trackDownloadTasks() {
			return trackDownloadTasks;
		},
		get playlistModalRef() {
			return playlistModalRef;
		},
		set playlistModalRef(v) {
			playlistModalRef = v;
		},
		jellyfinCallbacks,
		localCallbacks,
		get localDownloadCallback() {
			return localDownloadCallback;
		},
		navidromeCallbacks,
		plexCallbacks,
		...eventHandlers,
		retryTracks,
		refreshAll,
		handleDeleted,
		playSourceTrack,
		getTrackContextMenuItems
	};
}
