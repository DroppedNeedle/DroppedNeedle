import {
	fetchLocalAlbumMatch,
	fetchRemoteAlbumMatch
} from '$lib/features/album/queries/albumMatch.svelte';
import { readIntegrationStatus } from '$lib/features/home/queries/home.svelte';
import { playerStore } from '$lib/features/player/state/player.svelte';
import { toastStore } from '$lib/stores/toast';
import {
	buildQueueItemsFromLocal,
	buildRemoteQueueItems,
	type TrackMeta
} from '$lib/features/player/playback/queueHelpers';
import type { QueueItem } from '$lib/features/player/playback/types';
import type { RemoteSource } from '$lib/features/remote-library/queries/types';

export interface AlbumCardMeta {
	mbid: string;
	albumName: string;
	artistName: string;
	coverUrl: string | null;
	artistId?: string;
}

type SourceResult = { source: 'local' | 'navidrome' | 'jellyfin' | 'plex'; items: QueueItem[] };

function trackMeta(meta: AlbumCardMeta): TrackMeta {
	return {
		albumId: meta.mbid,
		albumName: meta.albumName,
		artistName: meta.artistName,
		coverUrl: meta.coverUrl,
		artistId: meta.artistId
	};
}

export async function fetchAlbumQueueItems(
	meta: AlbumCardMeta,
	signal?: AbortSignal
): Promise<QueueItem[]> {
	const status = readIntegrationStatus();
	const hints = { title: meta.albumName, artist: meta.artistName };
	const probes: Promise<SourceResult | null>[] = [];

	if (status?.localfiles) {
		probes.push(
			fetchLocalAlbumMatch(meta.mbid, signal)
				.then((data) => {
					if (!data?.found || data.tracks.length === 0) return null;
					return {
						source: 'local' as const,
						items: buildQueueItemsFromLocal(data.tracks, trackMeta(meta))
					};
				})
				.catch(() => null)
		);
	}

	const remotes: RemoteSource[] = ['navidrome', 'jellyfin', 'plex'];
	for (const source of remotes) {
		if (!status?.[source]) continue;
		probes.push(
			fetchRemoteAlbumMatch(source, meta.mbid, hints, signal)
				.then((data) => {
					if (!data.found || data.tracks.length === 0) return null;
					return { source, items: buildRemoteQueueItems(data.tracks, trackMeta(meta)) };
				})
				.catch(() => null)
		);
	}

	if (probes.length === 0) return [];

	const results = await Promise.all(probes);
	const priority: Array<'local' | 'navidrome' | 'jellyfin' | 'plex'> = [
		'local',
		'navidrome',
		'jellyfin',
		'plex'
	];
	for (const src of priority) {
		const hit = results.find((r) => r?.source === src);
		if (hit) return hit.items;
	}

	return [];
}

export async function cardQuickPlay(meta: AlbumCardMeta, signal?: AbortSignal): Promise<boolean> {
	const items = await fetchAlbumQueueItems(meta, signal);
	if (items.length === 0) {
		toastStore.notify('Nothing here can be played right now', 'info');
		return false;
	}
	playerStore.playQueue(items, 0, false);
	return true;
}

export async function cardQuickShuffle(
	meta: AlbumCardMeta,
	signal?: AbortSignal
): Promise<boolean> {
	const items = await fetchAlbumQueueItems(meta, signal);
	if (items.length === 0) {
		toastStore.notify('Nothing here can be played right now', 'info');
		return false;
	}
	playerStore.playQueue(items, 0, true);
	return true;
}

export async function cardAddToQueue(meta: AlbumCardMeta, signal?: AbortSignal): Promise<boolean> {
	const items = await fetchAlbumQueueItems(meta, signal);
	if (items.length === 0) {
		toastStore.notify('Nothing here can be played right now', 'info');
		return false;
	}
	playerStore.addMultipleToQueue(items);
	return true;
}

export async function cardPlayNext(meta: AlbumCardMeta, signal?: AbortSignal): Promise<boolean> {
	const items = await fetchAlbumQueueItems(meta, signal);
	if (items.length === 0) {
		toastStore.notify('Nothing here can be played right now', 'info');
		return false;
	}
	playerStore.playMultipleNext(items);
	return true;
}
