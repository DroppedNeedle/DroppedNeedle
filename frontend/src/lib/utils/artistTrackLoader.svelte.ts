import type { QueueItem } from '$lib/player/types';
import type { SourceType } from '$lib/player/types';
import type { ReleaseGroup } from '$lib/types';
import type {
	JellyfinAlbumMatch,
	LocalAlbumMatch,
	NavidromeAlbumMatch,
	PlexAlbumMatch
} from '$lib/types';
import {
	buildQueueItemsFromLocal,
	buildQueueItemsFromNavidrome,
	buildQueueItemsFromJellyfin,
	buildQueueItemsFromPlex
} from '$lib/player/queueHelpers';
import type { TrackMeta } from '$lib/player/queueHelpers';
import { API } from '$lib/constants';
import { getApiUrl } from '$lib/api/api-utils';
import { api } from '$lib/api/client';
import { getCoverUrl } from '$lib/utils/errorHandling';

type PlaybackSourceType = Exclude<SourceType, 'youtube'>;

const CONCURRENCY = 3;
const EARLY_PLAY_THRESHOLD = 3;

function createSemaphore(limit: number) {
	let running = 0;
	const queue: (() => void)[] = [];
	return <T>(fn: () => Promise<T>): Promise<T> =>
		new Promise((resolve, reject) => {
			const run = () => {
				running++;
				fn()
					.then(resolve, reject)
					.finally(() => {
						running--;
						queue.shift()?.();
					});
			};
			if (running < limit) run();
			else queue.push(run);
		});
}

async function fetchAlbumTracksForSource(
	release: ReleaseGroup,
	artistName: string,
	source: PlaybackSourceType,
	signal: AbortSignal
): Promise<QueueItem[]> {
	const mbid = release.id;
	const meta: TrackMeta = {
		albumId: mbid,
		albumName: release.title,
		artistName,
		coverUrl: getCoverUrl(null, mbid)
	};

	try {
		switch (source) {
			case 'local': {
				const match = await api.global.get<LocalAlbumMatch>(API.local.albumMatch(mbid), {
					signal
				});
				if (!match?.found || !match.tracks.length) return [];
				return buildQueueItemsFromLocal(match.tracks, meta);
			}
			case 'navidrome': {
				const url = new URL(
					getApiUrl(API.navidromeLibrary.albumMatch(mbid)),
					window.location.origin
				);
				url.searchParams.set('name', release.title);
				url.searchParams.set('artist', artistName);
				const match = await api.global.get<NavidromeAlbumMatch>(url.toString(), { signal });
				if (!match?.found || !match.tracks.length) return [];
				return buildQueueItemsFromNavidrome(match.tracks, meta);
			}
			case 'jellyfin': {
				const match = await api.global.get<JellyfinAlbumMatch>(
					API.jellyfinLibrary.albumMatch(mbid),
					{ signal }
				);
				if (!match?.found || !match.tracks.length) return [];
				return buildQueueItemsFromJellyfin(match.tracks, meta);
			}
			case 'plex': {
				const url = new URL(getApiUrl(API.plexLibrary.albumMatch(mbid)), window.location.origin);
				url.searchParams.set('name', release.title);
				url.searchParams.set('artist', artistName);
				const match = await api.global.get<PlexAlbumMatch>(url.toString(), { signal });
				if (!match?.found || !match.tracks.length) return [];
				return buildQueueItemsFromPlex(match.tracks, meta);
			}
		}
	} catch {
		if (signal.aborted) throw new DOMException('Aborted', 'AbortError');
		return [];
	}
}

export interface ArtistTrackLoaderConfig {
	artistName: string;
	artistId: string;
	releases: ReleaseGroup[];
	source: PlaybackSourceType;
}

export function createArtistTrackLoader(
	config: () => ArtistTrackLoaderConfig,
	onPlayQueue: (items: QueueItem[], startIndex: number, shuffle: boolean) => void,
	onQueueAppend: (items: QueueItem[]) => void,
	onShuffleRegenerate: () => void,
	onToast: (message: string, type: 'info' | 'error') => void
) {
	let controller: AbortController | null = null;
	let loading = $state(false);
	let loadedAlbums = $state(0);
	let totalAlbums = $state(0);
	let loadedTracks = $state(0);

	function abort() {
		controller?.abort();
		controller = null;
		loading = false;
	}

	function getInLibraryReleases(): ReleaseGroup[] {
		const { releases } = config();
		return releases.filter((r) => r.in_library).sort((a, b) => (a.year ?? 9999) - (b.year ?? 9999));
	}

	async function fetchAllTracks(signal: AbortSignal): Promise<{
		tracks: QueueItem[];
		matchedAlbums: number;
		totalAlbums: number;
	}> {
		const { artistName, source } = config();
		const inLibrary = getInLibraryReleases();
		const sem = createSemaphore(CONCURRENCY);

		totalAlbums = inLibrary.length;
		loadedAlbums = 0;
		loadedTracks = 0;

		const results = new Map<string, QueueItem[]>(); // eslint-disable-line svelte/prefer-svelte-reactivity

		await Promise.all(
			inLibrary.map((release) =>
				sem(async () => {
					if (signal.aborted) return;
					const items = await fetchAlbumTracksForSource(release, artistName, source, signal);
					results.set(release.id, items);
					loadedAlbums++;
					loadedTracks += items.length;
				})
			)
		);

		// Concatenate in year-sorted order
		const ordered: QueueItem[] = [];
		for (const release of inLibrary) {
			const items = results.get(release.id);
			if (items?.length) ordered.push(...items);
		}

		const matched = [...results.values()].filter((items) => items.length > 0).length;
		return { tracks: ordered, matchedAlbums: matched, totalAlbums: inLibrary.length };
	}

	async function fetchTracksProgressive(signal: AbortSignal, shuffle: boolean): Promise<void> {
		const { artistName, source } = config();
		const inLibrary = getInLibraryReleases();
		const sem = createSemaphore(CONCURRENCY);

		totalAlbums = inLibrary.length;
		loadedAlbums = 0;
		loadedTracks = 0;

		const results = new Map<string, QueueItem[]>(); // eslint-disable-line svelte/prefer-svelte-reactivity
		let playbackStarted = false;
		let resolvedCount = 0;

		const startPlaybackIfReady = () => {
			if (playbackStarted) return;
			if (resolvedCount < Math.min(EARLY_PLAY_THRESHOLD, inLibrary.length)) return;

			// Build queue from what's resolved so far, in release order
			const earlyQueue: QueueItem[] = [];
			for (const release of inLibrary) {
				const items = results.get(release.id);
				if (items?.length) earlyQueue.push(...items);
			}

			if (earlyQueue.length > 0) {
				playbackStarted = true;
				onPlayQueue(earlyQueue, 0, shuffle);
			}
		};

		await Promise.all(
			inLibrary.map((release) =>
				sem(async () => {
					if (signal.aborted) return;
					const items = await fetchAlbumTracksForSource(release, artistName, source, signal);
					results.set(release.id, items);
					resolvedCount++;
					loadedAlbums = resolvedCount;
					loadedTracks += items.length;

					if (!playbackStarted) {
						startPlaybackIfReady();
					} else if (items.length > 0) {
						onQueueAppend(items);
					}
				})
			)
		);

		if (signal.aborted) return;

		// If playback never started (e.g., fewer than EARLY_PLAY_THRESHOLD albums total)
		if (!playbackStarted) {
			const allTracks: QueueItem[] = [];
			for (const release of inLibrary) {
				const items = results.get(release.id);
				if (items?.length) allTracks.push(...items);
			}
			if (allTracks.length > 0) {
				onPlayQueue(allTracks, 0, shuffle);
			}
		}

		// Final shuffle regeneration after all batches (only if we actually started playback)
		if (shuffle && playbackStarted) onShuffleRegenerate();

		const matched = [...results.values()].filter((v) => v.length > 0).length;
		if (matched === 0) {
			onToast(`No playable tracks found from ${source}`, 'error');
		} else if (matched < inLibrary.length) {
			onToast(
				`Playing ${loadedTracks} tracks from ${matched} of ${inLibrary.length} albums (${inLibrary.length - matched} not found on ${source})`,
				'info'
			);
		}
	}

	function playAll() {
		abort();
		const inLibrary = getInLibraryReleases();
		if (inLibrary.length === 0) return;

		const ac = new AbortController();
		controller = ac;
		loading = true;

		fetchTracksProgressive(ac.signal, false)
			.catch((e: unknown) => {
				if (e instanceof DOMException && e.name === 'AbortError') return;
				throw e;
			})
			.finally(() => {
				if (controller === ac) {
					loading = false;
					controller = null;
				}
			});
	}

	function shuffleAll() {
		abort();
		const inLibrary = getInLibraryReleases();
		if (inLibrary.length === 0) return;

		const ac = new AbortController();
		controller = ac;
		loading = true;

		fetchTracksProgressive(ac.signal, true)
			.catch((e: unknown) => {
				if (e instanceof DOMException && e.name === 'AbortError') return;
				throw e;
			})
			.finally(() => {
				if (controller === ac) {
					loading = false;
					controller = null;
				}
			});
	}

	async function fetchTracks(): Promise<QueueItem[]> {
		abort();
		const inLibrary = getInLibraryReleases();
		if (inLibrary.length === 0) return [];

		const ac = new AbortController();
		controller = ac;
		loading = true;

		try {
			const result = await fetchAllTracks(ac.signal);
			if (result.matchedAlbums === 0) {
				onToast(`No playable tracks found from ${config().source}`, 'error');
			}
			return result.tracks;
		} finally {
			if (controller === ac) {
				loading = false;
				controller = null;
			}
		}
	}

	function getProgressText(): string | null {
		if (!loading) return null;
		if (totalAlbums === 0) return 'Loading...';
		return `Loaded ${loadedAlbums} of ${totalAlbums} albums (${loadedTracks} tracks)`;
	}

	return {
		get loading() {
			return loading;
		},
		get loadedAlbums() {
			return loadedAlbums;
		},
		get totalAlbums() {
			return totalAlbums;
		},
		get loadedTracks() {
			return loadedTracks;
		},
		get progressText() {
			return getProgressText();
		},
		playAll,
		shuffleAll,
		fetchTracks,
		abort
	};
}
