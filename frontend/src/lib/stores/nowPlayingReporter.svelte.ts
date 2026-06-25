import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { playerStore } from '$lib/stores/player.svelte';

// The web player reports presence only for native content. jellyfin/navidrome/plex
// playback is surfaced to other users by the server-side poll of those upstream
// servers, so reporting it here too would show a duplicate card.
const REPORTED_SOURCES = new Set(['local', 'youtube']);
const HEARTBEAT_MS = 5_000;

function createNowPlayingReporter() {
	let timer: ReturnType<typeof setInterval> | undefined;
	let cleared = true;

	function reportable(): boolean {
		const np = playerStore.nowPlaying;
		const state = playerStore.playbackState;
		return (
			!!np &&
			!!np.trackName &&
			REPORTED_SOURCES.has(np.sourceType) &&
			state !== 'idle' &&
			state !== 'error'
		);
	}

	async function report(): Promise<void> {
		const np = playerStore.nowPlaying;
		if (!np) return;
		const state = playerStore.playbackState;
		try {
			await api.global.post(API.nowPlaying.report(), {
				track_name: np.trackName ?? '',
				artist_name: np.artistName ?? '',
				album_name: np.albumName ?? null,
				cover_url: np.coverUrl ?? '',
				source: np.sourceType,
				device: 'web',
				is_paused: state === 'paused' || state === 'buffering' || state === 'loading',
				progress_ms: Math.round(playerStore.progress * 1000),
				duration_ms: Math.round(playerStore.duration * 1000)
			});
			cleared = false;
		} catch {
			// presence is best-effort; a dropped heartbeat just lets the entry TTL out
		}
	}

	async function clear(): Promise<void> {
		try {
			await api.global.delete(API.nowPlaying.report());
		} catch {
			// best-effort
		}
		cleared = true;
	}

	function tick(): void {
		if (reportable()) {
			void report();
		} else if (!cleared) {
			void clear();
		}
	}

	function start(): void {
		if (timer) return;
		cleared = true;
		tick();
		timer = setInterval(tick, HEARTBEAT_MS);
	}

	function stop(): void {
		if (timer) {
			clearInterval(timer);
			timer = undefined;
		}
		void clear();
	}

	return { start, stop };
}

export const nowPlayingReporter = createNowPlayingReporter();
