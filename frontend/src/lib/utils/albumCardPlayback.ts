import { fetchLocalAlbumMatch, fetchRemoteAlbumMatch } from '$lib/queries/albumMatch';
import { get } from 'svelte/store';
import { gatewayStreamUrl } from '$lib/player/playbackGateway';
import { integrationStore } from '$lib/stores/integration';
import { playerStore } from '$lib/stores/player.svelte';
import { playbackToast } from '$lib/stores/playbackToast.svelte';
import { recordingFor, REMOTE_FORMAT, remoteStreamKey } from '$lib/player/queueHelpers';
import { getCoverUrl } from '$lib/utils/errorHandling';
import type { LocalTrackInfo, QueueItem } from '$lib/player/types';
import type { RemoteSource, RemoteTrack } from '$lib/queries/remotes/types';

export interface AlbumCardMeta {
	mbid: string;
	albumName: string;
	artistName: string;
	coverUrl: string | null;
	artistId?: string;
}

type SourceResult = { source: 'local' | 'navidrome' | 'jellyfin' | 'plex'; items: QueueItem[] };

function buildLocalItems(tracks: LocalTrackInfo[], meta: AlbumCardMeta): QueueItem[] {
	const cover = getCoverUrl(meta.coverUrl, meta.mbid);
	return tracks.map((t) => ({
		trackSourceId: String(t.track_file_id),
		trackName: t.title,
		artistName: meta.artistName,
		trackNumber: t.track_number,
		albumId: meta.mbid,
		albumName: meta.albumName,
		coverUrl: cover,
		sourceType: 'local' as const,
		artistId: meta.artistId,
		streamUrl: gatewayStreamUrl('local', t.track_file_id),
		format: t.format.toLowerCase()
	}));
}

function buildRemoteItems(tracks: RemoteTrack[], meta: AlbumCardMeta): QueueItem[] {
	const cover = getCoverUrl(meta.coverUrl, meta.mbid);
	return tracks.flatMap((t) => {
		const key = remoteStreamKey(t);
		if (!key) return [];
		return [
			{
				trackSourceId: key,
				trackName: t.title,
				artistName: meta.artistName,
				trackNumber: t.track_number ?? 0,
				albumId: meta.mbid,
				albumName: meta.albumName,
				coverUrl: cover,
				sourceType: t.source,
				artistId: meta.artistId,
				streamUrl: gatewayStreamUrl(t.source, key),
				format: REMOTE_FORMAT,
				recordingIds: recordingFor(t.source, t.recording_mbid),
				plexRatingKey: t.source === 'plex' ? t.id : undefined
			}
		];
	});
}

export async function fetchAlbumQueueItems(
	meta: AlbumCardMeta,
	signal?: AbortSignal
): Promise<QueueItem[]> {
	const status = get(integrationStore);
	const hints = { title: meta.albumName, artist: meta.artistName };
	const probes: Promise<SourceResult | null>[] = [];

	if (status.localfiles) {
		probes.push(
			fetchLocalAlbumMatch(meta.mbid, signal)
				.then((data) => {
					if (!data?.found || data.tracks.length === 0) return null;
					return { source: 'local' as const, items: buildLocalItems(data.tracks, meta) };
				})
				.catch(() => null)
		);
	}

	const remotes: RemoteSource[] = ['navidrome', 'jellyfin', 'plex'];
	for (const source of remotes) {
		if (!status[source]) continue;
		probes.push(
			fetchRemoteAlbumMatch(source, meta.mbid, hints, signal)
				.then((data) => {
					if (!data.found || data.tracks.length === 0) return null;
					return { source, items: buildRemoteItems(data.tracks, meta) };
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
		playbackToast.show('Nothing here can be played right now', 'info');
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
		playbackToast.show('Nothing here can be played right now', 'info');
		return false;
	}
	playerStore.playQueue(items, 0, true);
	return true;
}

export async function cardAddToQueue(meta: AlbumCardMeta, signal?: AbortSignal): Promise<boolean> {
	const items = await fetchAlbumQueueItems(meta, signal);
	if (items.length === 0) {
		playbackToast.show('Nothing here can be played right now', 'info');
		return false;
	}
	playerStore.addMultipleToQueue(items);
	return true;
}

export async function cardPlayNext(meta: AlbumCardMeta, signal?: AbortSignal): Promise<boolean> {
	const items = await fetchAlbumQueueItems(meta, signal);
	if (items.length === 0) {
		playbackToast.show('Nothing here can be played right now', 'info');
		return false;
	}
	playerStore.playMultipleNext(items);
	return true;
}
