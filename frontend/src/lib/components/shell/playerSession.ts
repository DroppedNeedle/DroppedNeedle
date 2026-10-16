// The player's keyboard shortcuts and the restore of the last session on sign-in.
import { playerStore } from '$lib/stores/player.svelte';
import { launchYouTubePlayback } from '$lib/player/launchYouTubePlayback';

/** Space plays or pauses, the arrows seek by 10 seconds and step the volume by 5. */
export function handlePlayerShortcut(e: KeyboardEvent): void {
	const tag = (e.target as HTMLElement)?.tagName;
	if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
	if (!playerStore.isPlayerVisible) return;

	switch (e.key) {
		case ' ':
			e.preventDefault();
			playerStore.togglePlay();
			break;
		case 'ArrowRight':
			e.preventDefault();
			playerStore.seekTo(Math.min(playerStore.progress + 10, playerStore.duration));
			break;
		case 'ArrowLeft':
			e.preventDefault();
			playerStore.seekTo(Math.max(playerStore.progress - 10, 0));
			break;
		case 'ArrowUp':
			e.preventDefault();
			playerStore.setVolume(playerStore.volume + 5);
			break;
		case 'ArrowDown':
			e.preventDefault();
			playerStore.setVolume(playerStore.volume - 5);
			break;
	}
}

export async function restorePlayerSession(): Promise<void> {
	const session = playerStore.restoreSession();
	if (!session) return;

	try {
		if (session.nowPlaying.sourceType === 'youtube') {
			if (!session.nowPlaying.trackSourceId) return;
			await launchYouTubePlayback({
				albumId: session.nowPlaying.albumId,
				albumName: session.nowPlaying.albumName,
				artistName: session.nowPlaying.artistName,
				coverUrl: session.nowPlaying.coverUrl,
				videoId: session.nowPlaying.trackSourceId,
				embedUrl: session.nowPlaying.embedUrl
			});
		} else {
			playerStore.resumeSession();
		}
	} catch {
		return;
	}
}
