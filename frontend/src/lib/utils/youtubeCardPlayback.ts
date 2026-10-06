import { YOUTUBE_ENDPOINTS } from '$lib/queries/youtube/endpoints';
import { api } from '$lib/api/client';
import { playerStore } from '$lib/stores/player.svelte';
import { playbackToast } from '$lib/stores/playbackToast.svelte';
import { buildQueueItemsFromYouTube, type TrackMeta } from '$lib/player/queueHelpers';
import { openGlobalPlaylistModal } from '$lib/stores/playlistModal.svelte';
import { getCoverUrl } from '$lib/utils/errorHandling';
import type { QueueItem } from '$lib/player/types';
import type { YouTubeLink } from '$lib/types';

type FetchResult = { items: QueueItem[]; error: boolean };

function buildMeta(link: YouTubeLink): TrackMeta {
	return {
		albumId: link.album_id,
		albumName: link.album_name,
		artistName: link.artist_name,
		coverUrl: getCoverUrl(link.cover_url, link.album_id)
	};
}

async function fetchTrackItems(link: YouTubeLink): Promise<FetchResult> {
	try {
		const tracks = await api.global.v3.GET(YOUTUBE_ENDPOINTS.trackLinks(link.album_id));
		if (tracks.length === 0) return { items: [], error: false };
		return { items: buildQueueItemsFromYouTube(tracks, buildMeta(link)), error: false };
	} catch {
		return { items: [], error: true };
	}
}

export async function ytCardQuickPlay(link: YouTubeLink): Promise<boolean> {
	const { items, error } = await fetchTrackItems(link);
	if (error) {
		playbackToast.show("Couldn't load the track list", 'error');
		return false;
	}
	if (items.length === 0) {
		playbackToast.show('No tracks linked yet', 'info');
		return false;
	}
	playerStore.playQueue(items, 0, false);
	return true;
}

export async function ytCardQuickShuffle(link: YouTubeLink): Promise<boolean> {
	const { items, error } = await fetchTrackItems(link);
	if (error) {
		playbackToast.show("Couldn't load the track list", 'error');
		return false;
	}
	if (items.length === 0) {
		playbackToast.show('No tracks linked yet', 'info');
		return false;
	}
	playerStore.playQueue(items, 0, true);
	return true;
}

export async function ytCardAddToQueue(link: YouTubeLink): Promise<boolean> {
	const { items, error } = await fetchTrackItems(link);
	if (error) {
		playbackToast.show("Couldn't load the track list", 'error');
		return false;
	}
	if (items.length === 0) {
		playbackToast.show('No tracks linked yet', 'info');
		return false;
	}
	playerStore.addMultipleToQueue(items);
	return true;
}

export async function ytCardPlayNext(link: YouTubeLink): Promise<boolean> {
	const { items, error } = await fetchTrackItems(link);
	if (error) {
		playbackToast.show("Couldn't load the track list", 'error');
		return false;
	}
	if (items.length === 0) {
		playbackToast.show('No tracks linked yet', 'info');
		return false;
	}
	playerStore.playMultipleNext(items);
	return true;
}

export async function ytCardAddToPlaylist(link: YouTubeLink): Promise<void> {
	const { items, error } = await fetchTrackItems(link);
	if (error) {
		playbackToast.show("Couldn't load the track list", 'error');
		return;
	}
	if (items.length === 0) {
		playbackToast.show('No tracks linked yet', 'info');
		return;
	}
	openGlobalPlaylistModal(items);
}
