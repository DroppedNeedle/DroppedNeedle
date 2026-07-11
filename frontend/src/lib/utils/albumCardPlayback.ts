import { get } from 'svelte/store';
import { API } from '$lib/constants';
import { getApiUrl } from '$lib/api/api-utils';
import { api } from '$lib/api/client';
import { integrationStore } from '$lib/stores/integration';
import { playerStore } from '$lib/stores/player.svelte';
import { playbackToast } from '$lib/stores/playbackToast.svelte';
import { normalizeCodec } from '$lib/player/queueHelpers';
import { getCoverUrl } from '$lib/utils/errorHandling';
import type { QueueItem } from '$lib/player/types';
import type {
	JellyfinAlbumMatch,
	LocalAlbumMatch,
	NavidromeAlbumMatch,
	PlexAlbumMatch,
	JellyfinTrackInfo,
	LocalTrackInfo,
	NavidromeTrackInfo,
	PlexTrackInfo
} from '$lib/types';

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
		streamUrl: API.stream.local(t.track_file_id),
		format: t.format.toLowerCase()
	}));
}

function buildNavidromeItems(tracks: NavidromeTrackInfo[], meta: AlbumCardMeta): QueueItem[] {
	const cover = getCoverUrl(meta.coverUrl, meta.mbid);
	return tracks.map((t) => ({
		trackSourceId: t.navidrome_id,
		trackName: t.title,
		artistName: meta.artistName,
		trackNumber: t.track_number,
		albumId: meta.mbid,
		albumName: meta.albumName,
		coverUrl: cover,
		sourceType: 'navidrome' as const,
		artistId: meta.artistId,
		streamUrl: API.stream.navidrome(t.navidrome_id),
		format: normalizeCodec(t.codec)
	}));
}

function buildJellyfinItems(tracks: JellyfinTrackInfo[], meta: AlbumCardMeta): QueueItem[] {
	const cover = getCoverUrl(meta.coverUrl, meta.mbid);
	return tracks.map((t) => ({
		trackSourceId: t.jellyfin_id,
		trackName: t.title,
		artistName: meta.artistName,
		trackNumber: t.track_number,
		albumId: meta.mbid,
		albumName: meta.albumName,
		coverUrl: cover,
		sourceType: 'jellyfin' as const,
		artistId: meta.artistId,
		streamUrl: API.stream.jellyfin(t.jellyfin_id),
		format: normalizeCodec(t.codec)
	}));
}

function buildPlexItems(tracks: PlexTrackInfo[], meta: AlbumCardMeta): QueueItem[] {
	const cover = getCoverUrl(meta.coverUrl, meta.mbid);
	return tracks
		.filter((t) => t.part_key)
		.map((t) => {
			return {
				trackSourceId: t.part_key!,
				trackName: t.title,
				artistName: meta.artistName,
				trackNumber: t.track_number,
				albumId: meta.mbid,
				albumName: meta.albumName,
				coverUrl: cover,
				sourceType: 'plex' as const,
				artistId: meta.artistId,
				streamUrl: API.stream.plex(t.part_key!),
				format: normalizeCodec(t.codec),
				plexRatingKey: t.plex_id
			};
		});
}

export async function fetchAlbumQueueItems(
	meta: AlbumCardMeta,
	signal?: AbortSignal
): Promise<QueueItem[]> {
	const status = get(integrationStore);
	const probes: Promise<SourceResult | null>[] = [];

	if (status.localfiles) {
		probes.push(
			api.global
				.get<LocalAlbumMatch>(API.local.albumMatch(meta.mbid), { signal })
				.then((data) => {
					if (!data?.found || data.tracks.length === 0) return null;
					return { source: 'local' as const, items: buildLocalItems(data.tracks, meta) };
				})
				.catch(() => null)
		);
	}

	if (status.navidrome) {
		const url = new URL(
			getApiUrl(API.navidromeLibrary.albumMatch(meta.mbid)),
			window.location.origin
		);
		if (meta.albumName) url.searchParams.set('name', meta.albumName);
		if (meta.artistName) url.searchParams.set('artist', meta.artistName);
		probes.push(
			api.global
				.get<NavidromeAlbumMatch>(url.toString(), { signal })
				.then((data) => {
					if (!data?.found || data.tracks.length === 0) return null;
					return {
						source: 'navidrome' as const,
						items: buildNavidromeItems(data.tracks, meta)
					};
				})
				.catch(() => null)
		);
	}

	if (status.jellyfin) {
		probes.push(
			api.global
				.get<JellyfinAlbumMatch>(API.jellyfinLibrary.albumMatch(meta.mbid), { signal })
				.then((data) => {
					if (!data?.found || data.tracks.length === 0) return null;
					return {
						source: 'jellyfin' as const,
						items: buildJellyfinItems(data.tracks, meta)
					};
				})
				.catch(() => null)
		);
	}

	if (status.plex) {
		const plexUrl = new URL(
			getApiUrl(API.plexLibrary.albumMatch(meta.mbid)),
			window.location.origin
		);
		if (meta.albumName) plexUrl.searchParams.set('name', meta.albumName);
		if (meta.artistName) plexUrl.searchParams.set('artist', meta.artistName);
		probes.push(
			api.global
				.get<PlexAlbumMatch>(plexUrl.toString(), { signal })
				.then((data) => {
					if (!data?.found || data.tracks.length === 0) return null;
					return {
						source: 'plex' as const,
						items: buildPlexItems(data.tracks, meta)
					};
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
