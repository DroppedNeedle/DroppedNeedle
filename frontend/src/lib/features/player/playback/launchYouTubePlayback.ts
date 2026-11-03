import { tick } from 'svelte';
import { playerStore } from '$lib/features/player/state/player.svelte';
import { createPlaybackSource } from '$lib/features/player/playback/createSource';
import { getCoverUrl } from '$lib/utils/errorHandling';

export type YouTubePlaybackPayload = {
	albumId: string;
	albumName: string;
	artistName: string;
	videoId: string;
	coverUrl?: string | null;
	embedUrl?: string;
	artistId?: string;
};

type LaunchYouTubePlaybackOptions = {
	onLoadError?: (error: unknown) => void;
	stopOnError?: boolean;
};

export async function launchYouTubePlayback(
	payload: YouTubePlaybackPayload,
	options: LaunchYouTubePlaybackOptions = {}
): Promise<void> {
	const { stopOnError = true, onLoadError } = options;
	const normalizedCoverUrl = getCoverUrl(payload.coverUrl ?? null, payload.albumId);

	const source = createPlaybackSource('youtube');
	playerStore.playAlbum(source, {
		albumId: payload.albumId,
		albumName: payload.albumName,
		artistName: payload.artistName,
		coverUrl: normalizedCoverUrl,
		sourceType: 'youtube',
		trackSourceId: payload.videoId,
		embedUrl: payload.embedUrl ?? `https://www.youtube.com/embed/${payload.videoId}`,
		artistId: payload.artistId
	});

	await tick();

	try {
		await source.load({ trackSourceId: payload.videoId });
	} catch (error) {
		if (stopOnError) {
			playerStore.stop();
		}
		onLoadError?.(error);
		throw error;
	}
}
