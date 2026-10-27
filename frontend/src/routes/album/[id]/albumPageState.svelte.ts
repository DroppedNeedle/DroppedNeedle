import { browser } from '$app/environment';
import { get } from 'svelte/store';
import { untrack } from 'svelte';
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { useQueryClient } from '@tanstack/svelte-query';
import type {
	AlbumBasicInfo,
	YouTubeTrackLink,
	YouTubeLink,
	YouTubeQuotaStatus,
	LocalAlbumMatch,
	LocalTrackInfo,
	LibraryAlbumStatus,
	LibraryFileMeta,
	DownloadTask,
	HeldFileInEntry
} from '$lib/types';
import type { RemoteTrack } from '$lib/queries/remotes/types';
import { libraryStore } from '$lib/stores/library';
import { integrationStore } from '$lib/stores/integration';
import { ApiError } from '$lib/api/client';
import { compareDiscTrack, getDiscTrackKey } from '$lib/player/queueHelpers';
import type { QueueItem } from '$lib/player/types';
import { launchRemotePlayback } from '$lib/player/launchRemotePlayback';
import { launchLocalPlayback } from '$lib/player/launchLocalPlayback';
import type { MenuItem } from '$lib/components/ContextMenu.svelte';
import { buildRenderedTrackSections, buildSortedTrackMap } from './albumTrackResolvers';
import { createEventHandlers } from './albumEventHandlers';
import {
	playSourceTrack as playSourceTrackImpl,
	getTrackContextMenuItems as getTrackContextMenuItemsImpl,
	buildLocalAlbumDownloadCallback,
	buildSourceCallbacks
} from './albumPlaybackHandlers';
import { getLibraryAlbumStatusQuery } from '$lib/queries/library/LibraryQueries.svelte';
import {
	editionFetchLive,
	getAlbumEditionStatusQuery,
	getEditionFetchQuery,
	getEditionPlanQuery,
	type EditionFetchTrackState
} from '$lib/queries/albums/EditionQueries.svelte';
import { editionTrackRows, unmatchedFiles, type EditionTrackRow } from './albumEdition';
import { getAlbumDownloadsQuery } from '$lib/queries/downloads/DownloadQueries.svelte';
import { getHeldEntriesQuery } from '$lib/queries/downloads/HeldQueries.svelte';
import { isActiveDownloadStatus } from '$lib/queries/downloads/downloadStatus';
import { authStore } from '$lib/stores/authStore.svelte';
import { removeLibraryTrack } from '$lib/queries/library/LibraryMutations.svelte';
import { toastStore } from '$lib/stores/toast';
import { getActiveRequestsQuery } from '$lib/queries/requests/RequestQueries.svelte';
import { switchRequestedEdition } from '$lib/queries/albums/EditionQueries.svelte';
import type { EditionRequestPick } from '$lib/components/library/edition/editionSheetState.svelte';
import {
	getAlbumBasicQuery,
	getAlbumLastFmQuery,
	getAlbumTracksQuery,
	getAlbumYouTubeLinkQuery,
	getAlbumYouTubeTrackLinksQuery,
	getMoreByArtistQuery,
	getSimilarAlbumsQuery
} from '$lib/queries/catalog/AlbumQueries.svelte';
import { AlbumQueryKeyFactory } from '$lib/queries/catalog/AlbumQueryKeyFactory';
import { refreshAlbum } from '$lib/queries/catalog/AlbumMutations.svelte';
import {
	AlbumMatchKeyFactory,
	getLocalAlbumMatchQuery,
	getRemoteAlbumMatchQuery
} from '$lib/queries/albumMatch';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';

/** An edition track row, with where the edition fetch is with it. */
export interface FetchedEditionRow extends EditionTrackRow {
	/** Set while a fetch that asked for the track runs, or after it failed. */
	fetchState: EditionFetchTrackState | null;
}

export interface SourceCallbacks {
	onPlayAll: () => void;
	onShuffle: () => void;
	onAddAllToQueue: () => void;
	onPlayAllNext: () => void;
	onAddAllToPlaylist: () => void;
}

export function createAlbumPageState(albumIdGetter: () => string) {
	const client = useQueryClient();
	const userId = () => authStore.user?.id;
	let integrations = $state(get(integrationStore));
	let requesting = $state(false);
	let showDeleteModal = $state(false);
	let quota = $state<YouTubeQuotaStatus | null>(null);
	let playlistModalRef = $state<{ open: (tracks: QueueItem[]) => void } | null>(null);
	let externalRecheckTimers: ReturnType<typeof setTimeout>[] = [];
	// task ids we've watched go active this session, and the ones whose completion we've already
	// handled - so an already-finished download present on first load doesn't trigger a spurious refresh
	const seenActiveTaskIds = new SvelteSet<string>();
	const settledTaskIds = new SvelteSet<string>();

	$effect(() => integrationStore.subscribe((s) => (integrations = s)));
	$effect(() => {
		if (browser) void integrationStore.ensureLoaded();
	});
	const downloadClientConfigured = $derived(integrations.download_client);

	const basicQuery = getAlbumBasicQuery(userId, albumIdGetter);
	const album = $derived<AlbumBasicInfo | null>(basicQuery.data ?? null);
	const primaryError = $derived(basicQuery.error instanceof ApiError ? basicQuery.error : null);
	const error = $derived(basicQuery.isError && !album ? 'Error loading album' : null);
	const tracksQuery = getAlbumTracksQuery(userId, albumIdGetter);
	const tracksInfo = $derived(tracksQuery.data ?? null);
	const tracksError = $derived(tracksQuery.isError && !tracksInfo);
	const renderedTrackSections = $derived(
		tracksInfo ? buildRenderedTrackSections(tracksInfo.tracks) : []
	);

	// The shelves wait for the tracklist, which the page shows first.
	const discoveryParams = () => ({
		albumId: albumIdGetter(),
		artistId: album?.artist_id ?? '',
		enabled: !tracksQuery.isPending
	});
	const moreByArtistQuery = getMoreByArtistQuery(userId, discoveryParams);
	const similarAlbumsQuery = getSimilarAlbumsQuery(userId, discoveryParams);
	const loadingDiscovery = $derived(
		!!album?.artist_id && (moreByArtistQuery.isPending || similarAlbumsQuery.isPending)
	);

	const lastFmQuery = getAlbumLastFmQuery(() =>
		album
			? { albumId: albumIdGetter(), artistName: album.artist_name, albumName: album.title }
			: null
	);
	const youtubeLinkQuery = getAlbumYouTubeLinkQuery(albumIdGetter);
	const youtubeTrackLinksQuery = getAlbumYouTubeTrackLinksQuery(albumIdGetter);
	const trackLinks = $derived(youtubeTrackLinksQuery.data ?? []);

	// Jellyfin and the local library match on the id alone; Navidrome and Plex
	// also take the title and artist, so they wait for the header.
	const localMatchQuery = getLocalAlbumMatchQuery(
		userId,
		albumIdGetter,
		() => integrations.localfiles
	);
	const jellyfinMatchQuery = getRemoteAlbumMatchQuery(userId, () => ({
		source: 'jellyfin',
		mbid: albumIdGetter(),
		hints: {},
		enabled: integrations.jellyfin
	}));
	const namedHints = () => ({ title: album?.title, artist: album?.artist_name });
	const navidromeMatchQuery = getRemoteAlbumMatchQuery(userId, () => ({
		source: 'navidrome',
		mbid: albumIdGetter(),
		hints: namedHints(),
		enabled: integrations.navidrome && !!album
	}));
	const plexMatchQuery = getRemoteAlbumMatchQuery(userId, () => ({
		source: 'plex',
		mbid: albumIdGetter(),
		hints: namedHints(),
		enabled: integrations.plex && !!album
	}));
	const localMatch = $derived<LocalAlbumMatch | null>(localMatchQuery.data ?? null);
	const jellyfinMatch = $derived(jellyfinMatchQuery.data ?? null);
	const navidromeMatch = $derived(navidromeMatchQuery.data ?? null);
	const plexMatch = $derived(plexMatchQuery.data ?? null);

	const refresh = refreshAlbum();

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
	const heldQuery = getHeldEntriesQuery(
		() => ({ releaseGroupMbid: albumIdGetter() }),
		() => downloadClientConfigured && !!albumIdGetter()
	);
	const heldFiles = $derived(
		(heldQuery.data?.entries ?? []).flatMap((entry) =>
			entry.files.map((file): HeldFileInEntry => ({ entry, file }))
		)
	);
	const headerHeldFiles = $derived.by(() => {
		if (!headerDownloadTask) return [];
		return heldFiles
			.filter((held) => held.file.source_task_id === headerDownloadTask?.id)
			.map((held) => held.file);
	});
	const heldByRecording = $derived.by(() => {
		const m = new SvelteMap<string, HeldFileInEntry>();
		for (const held of heldFiles) {
			if (held.file.recording_mbid) m.set(held.file.recording_mbid, held);
		}
		return m;
	});
	const heldByPosition = $derived.by(() => {
		const m = new SvelteMap<string, HeldFileInEntry>();
		for (const held of heldFiles) {
			m.set(
				getDiscTrackKey({
					disc_number: held.file.disc_number,
					track_number: held.file.track_number
				}),
				held
			);
		}
		return m;
	});
	// A held file resolving (import or discard) shrinks the held set. Refetch
	// library status + source matches so a newly-owned track's play button appears in place,
	// without a manual refresh (mirrors the settled-download effect above).
	let lastHeldItemCount = -1;
	$effect(() => {
		const count = heldFiles.length;
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
	// Coverage: what the held files actually COVER of the release's tracklist.
	// expectedTracks === 0 -> tracklist unavailable, badge/Play All fall back to the
	// presence-only reading (never block playback on a MusicBrainz hiccup).
	const coverageExpected = $derived(libraryStatus?.expected_tracks ?? 0);
	const matchedFileIds = $derived(new SvelteSet(libraryStatus?.matched_file_ids ?? []));

	// The local album that holds this release group, and its current edition's
	// plan: the same reads, under the same keys, as the edition plate, so the
	// track rows, the counts and the plate never disagree.
	const localAlbumId = $derived(
		libraryStatus && libraryStatus.album_id !== albumIdGetter() ? libraryStatus.album_id : ''
	);
	const editionStatusQuery = getAlbumEditionStatusQuery(userId, () => localAlbumId);
	const editionPlanQuery = getEditionPlanQuery(
		userId,
		() => localAlbumId,
		() => null,
		() =>
			editionStatusQuery.data?.album_id === localAlbumId &&
			editionStatusQuery.data.edition_kind === 'release'
	);
	const editionPlan = $derived(
		localAlbumId && editionPlanQuery.data?.album_id === localAlbumId ? editionPlanQuery.data : null
	);
	const libraryTracksById = $derived(
		new SvelteMap((libraryStatus?.tracks ?? []).map((track) => [track.id, track]))
	);
	// The edition's tracks a fetch asked for, while it runs or after it
	// failed: their rows say fetching, failed or held instead of missing.
	const editionFetchQuery = getEditionFetchQuery(
		userId,
		() => editionPlan?.release_group_mbid ?? '',
		() => localAlbumId,
		() => authStore.isTrusted
	);
	const fetchStates = $derived.by(() => {
		const edition = editionFetchQuery.data?.latest;
		if (!edition || !editionPlan || edition.release_mbid !== editionPlan.release_mbid) return null;
		if (!editionFetchLive(edition) && edition.state !== 'failed') return null;
		return new SvelteMap(
			edition.tracks.map((track) => [
				getDiscTrackKey({ disc_number: track.disc, position: track.position }),
				track.state
			])
		);
	});
	// The rows follow the plan only while the page lists the same edition.
	const editionRows = $derived.by((): Map<string, FetchedEditionRow> | null => {
		if (!editionPlan || editionPlan.release_mbid !== tracksInfo?.selected_release_mbid) return null;
		const rows = new SvelteMap<string, FetchedEditionRow>();
		for (const [key, row] of editionTrackRows(editionPlan, libraryTracksById)) {
			rows.set(key, { ...row, fetchState: fetchStates?.get(key) ?? null });
		}
		return rows;
	});
	const editionCounts = $derived(
		editionPlan ? { kept: editionPlan.kept_count, total: editionPlan.tracks.length } : null
	);
	const libraryUnmatchedFiles = $derived(
		editionPlan ? unmatchedFiles(editionPlan, libraryTracksById) : []
	);

	function clearExternalRecheck() {
		for (const t of externalRecheckTimers) clearTimeout(t);
		externalRecheckTimers = [];
	}

	function refreshSourcesAfterDownload(): void {
		const albumId = albumIdGetter();
		if (!albumId) return;
		void invalidateQueriesWithPersister(
			{ queryKey: AlbumMatchKeyFactory.album(userId(), albumId) },
			undefined,
			{ client }
		);
	}

	// external servers (Jellyfin/Navidrome/Plex) index newly-imported files on their own schedule, so
	// re-check a couple of times after a download lands, then stop. Local Files is the native library
	// and is already fresh from the immediate refresh.
	function scheduleExternalSourceRecheck(): void {
		clearExternalRecheck();
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

	async function refreshAll(): Promise<void> {
		const albumId = albumIdGetter();
		if (!albumId || refresh.isPending) return;
		await refresh.mutateAsync({ userId: userId(), albumId }).catch(() => undefined);
	}

	function setAlbum(next: AlbumBasicInfo): void {
		void setQueryDataWithPersister(
			AlbumQueryKeyFactory.basic(userId(), albumIdGetter()),
			next,
			undefined,
			client
		);
	}

	function handleDeleted(): void {
		showDeleteModal = false;
		const albumId = albumIdGetter();
		if (album) setAlbum({ ...album, in_library: false, requested: false });
		const localKey = AlbumMatchKeyFactory.local(userId(), albumId);
		void setQueryDataWithPersister<LocalAlbumMatch>(
			localKey,
			{ found: false, musicbrainz_id: albumId, tracks: [], total_size_bytes: 0 },
			undefined,
			client
		);
		toastStore.show({ message: 'Removed from Library', type: 'success' });
	}

	// A new album starts with a clean slate of page-only state.
	$effect(() => {
		const albumId = albumIdGetter();
		if (!albumId) return;
		untrack(() => {
			pickedRelease = null;
			quota = null;
			clearExternalRecheck();
			seenActiveTaskIds.clear();
			settledTaskIds.clear();
			lastHeldItemCount = -1;
		});
		return clearExternalRecheck;
	});

	// The edition a request gets on an album the library does not hold
	// (curators): their pick, else the catalog's; once requested, the live
	// request's, which a pick switches until its download starts.
	let pickedRelease = $state<string | null>(null);
	const activeRequestsQuery = getActiveRequestsQuery(() => isRequested && authStore.isTrusted);
	const liveRequest = $derived(
		activeRequestsQuery.data?.items.find(
			(item) =>
				item.request_kind === 'album' && item.musicbrainz_id === album?.musicbrainz_id.toLowerCase()
		) ?? null
	);
	const switchEdition = switchRequestedEdition();
	const editionPick = $derived.by((): EditionRequestPick | null => {
		if (!album || inLibrary || libraryInLibrary || !authStore.isTrusted) return null;
		if (isRequested && !liveRequest) return null;
		const current = album;
		return {
			release: liveRequest
				? (liveRequest.release_mbid ?? null)
				: (pickedRelease ?? tracksInfo?.selected_release_mbid ?? null),
			requested: isRequested,
			locked: Boolean(liveRequest?.task_id),
			albumTitle: current.title,
			pick: async (release) => {
				if (!isRequested) {
					pickedRelease = release;
					return;
				}
				const answer = await switchEdition.mutateAsync({
					group: current.musicbrainz_id,
					release,
					artistName: current.artist_name ?? null,
					albumTitle: current.title
				});
				toastStore.show({
					message: answer.message,
					type: answer.status === 'downloading' ? 'info' : 'success'
				});
			}
		};
	});

	const eventHandlers = createEventHandlers({
		getAlbum: () => album,
		setAlbum,
		getSelectedRelease: () => pickedRelease ?? tracksInfo?.selected_release_mbid ?? null,
		setTrackLinks: (tl) =>
			void setQueryDataWithPersister<YouTubeTrackLink[]>(
				AlbumQueryKeyFactory.youtubeTrackLinks(albumIdGetter()),
				tl,
				undefined,
				client
			),
		getTrackLinks: () => trackLinks,
		setAlbumLink: (l) =>
			void setQueryDataWithPersister<YouTubeLink | null>(
				AlbumQueryKeyFactory.youtubeLink(albumIdGetter()),
				l,
				undefined,
				client
			),
		setQuota: (q) => (quota = q),
		setRequesting: (v) => (requesting = v),
		getRequesting: () => requesting,
		setShowDeleteModal: (v) => (showDeleteModal = v),
		onRequestSuccess: () => {
			// the sources' copies are read again on the next visit
			void invalidateQueriesWithPersister(
				{ queryKey: AlbumMatchKeyFactory.album(userId(), albumIdGetter()), refetchType: 'none' },
				undefined,
				{ client }
			).catch(() => undefined);
			// pick up the freshly-created download task so the header strip + polling kick in
			void downloadsQuery.refetch();
		}
	});

	function retryTracks(): void {
		void tracksQuery.refetch();
	}

	// Playback sees only files that COVER an expected track: an orphan
	// ("doesn't match this album") never enters the album queue - it plays
	// individually from the review section. Falls back to the raw match when
	// the tracklist was unavailable (expected_tracks === 0).
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
			localMatchForPlayback,
			navidromeMatch,
			plexMatch,
			tracksInfo?.tracks ?? []
		);
	}

	function getTrackContextMenuItems(
		track: { position: number; disc_number?: number | null; title: string },
		resolvedLocal: LocalTrackInfo | null,
		resolvedJellyfin: RemoteTrack | null,
		resolvedNavidrome: RemoteTrack | null = null,
		resolvedPlex: RemoteTrack | null = null
	): MenuItem[] {
		if (!album) return [];
		return getTrackContextMenuItemsImpl(
			track,
			album,
			resolvedLocal,
			resolvedJellyfin,
			resolvedNavidrome,
			resolvedPlex,
			playlistModalRef,
			localMatch?.download_allowed !== false,
			authStore.isTrusted
				? (fileId: string) => openRemoveFileDialog(fileId, track.title)
				: undefined
		);
	}

	const removeTrack = removeLibraryTrack();
	let removeFileTarget = $state<{
		fileId: string;
		albumMbid: string;
		albumCacheId: string;
		title: string;
	} | null>(null);
	let removeFileError = $state<string | null>(null);

	// A route change must not leave the old dialog (and its stale ids) on screen.
	$effect(() => {
		const activeAlbumId = albumIdGetter();
		if (removeFileTarget && removeFileTarget.albumCacheId !== activeAlbumId) {
			removeFileTarget = null;
			removeFileError = null;
		}
	});

	function openRemoveFileDialog(fileId: string, title: string): void {
		const albumMbid = album?.musicbrainz_id ?? '';
		const albumCacheId = albumIdGetter();
		if (!albumMbid || !fileId || !albumCacheId) return;
		removeFileError = null;
		removeFileTarget = { fileId, albumMbid, albumCacheId, title };
	}

	function closeRemoveFileDialog(): void {
		if (removeTrack.isPending) return;
		removeFileTarget = null;
		removeFileError = null;
	}

	function confirmRemoveFile(): void {
		const target = removeFileTarget;
		if (!target || removeTrack.isPending) return;
		removeTrack.mutate(
			{ fileId: target.fileId, albumMbid: target.albumMbid },
			{
				onSuccess: () => {
					removeFileTarget = null;
					removeFileError = null;
					toastStore.show({ message: 'File removed', type: 'success' });
				},
				onError: () => {
					removeFileError = "Couldn't remove this file";
					toastStore.show({ message: "Couldn't remove this file", type: 'error' });
				}
			}
		);
	}

	const localDownloadCallback = $derived<{ callback: (() => void) | undefined }>({
		callback: buildLocalAlbumDownloadCallback(
			localMatch?.musicbrainz_id,
			localMatch?.download_allowed !== false
		)
	});

	const jellyfinCallbacks: SourceCallbacks = buildSourceCallbacks(
		() => jellyfinMatch,
		launchRemotePlayback,
		'jellyfin',
		albumGetter,
		tracksGetters,
		() => tracksInfo?.tracks ?? [],
		playlistRefGetter
	);
	const localCallbacks: SourceCallbacks = buildSourceCallbacks(
		() => localMatchForPlayback,
		launchLocalPlayback,
		'local',
		albumGetter,
		tracksGetters,
		() => tracksInfo?.tracks ?? [],
		playlistRefGetter
	);
	const navidromeCallbacks: SourceCallbacks = buildSourceCallbacks(
		() => navidromeMatch,
		launchRemotePlayback,
		'navidrome',
		albumGetter,
		tracksGetters,
		() => tracksInfo?.tracks ?? [],
		playlistRefGetter
	);
	const plexCallbacks: SourceCallbacks = buildSourceCallbacks(
		() => plexMatch,
		launchRemotePlayback,
		'plex',
		albumGetter,
		tracksGetters,
		() => tracksInfo?.tracks ?? [],
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
		get primaryError() {
			return primaryError;
		},
		get loadingBasic() {
			return basicQuery.isPending;
		},
		get loadingTracks() {
			return tracksQuery.isPending;
		},
		get tracksError() {
			return tracksError;
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
		get removeFileTarget() {
			return removeFileTarget;
		},
		get removeFilePending() {
			return removeTrack.isPending;
		},
		get removeFileError() {
			return removeFileError;
		},
		openRemoveFileDialog,
		closeRemoveFileDialog,
		confirmRemoveFile,
		get moreByArtist() {
			return moreByArtistQuery.data ?? null;
		},
		get similarAlbums() {
			return similarAlbumsQuery.data ?? null;
		},
		get loadingDiscovery() {
			return loadingDiscovery;
		},
		get trackLinks() {
			return trackLinks;
		},
		get albumLink() {
			return youtubeLinkQuery.data ?? null;
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
			return jellyfinMatchQuery.isLoading;
		},
		get loadingLocal() {
			return localMatchQuery.isLoading;
		},
		get loadingNavidrome() {
			return navidromeMatchQuery.isLoading;
		},
		get loadingPlex() {
			return plexMatchQuery.isLoading;
		},
		get lastfmEnrichment() {
			return lastFmQuery.data ?? null;
		},
		get loadingLastfm() {
			return lastFmQuery.isPending;
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
		get editionPick() {
			return editionPick;
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
		get localMatchForPlayback() {
			return localMatchForPlayback;
		},
		get localAlbumId() {
			return localAlbumId;
		},
		get editionRows() {
			return editionRows;
		},
		get editionCounts() {
			return editionCounts;
		},
		get libraryUnmatchedFiles() {
			return libraryUnmatchedFiles;
		},
		get refreshing() {
			return refresh.isPending;
		},
		get headerDownloadTask() {
			return headerDownloadTask;
		},
		get headerHeldFiles() {
			return headerHeldFiles;
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
