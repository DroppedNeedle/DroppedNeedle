import { nowPlayingStore } from '$lib/stores/nowPlayingSessions.svelte';
import { playerStore } from '$lib/stores/player.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { mergeNowPlayingSessions } from '$lib/stores/nowPlayingMerge';
import type { NowPlayingSession } from '$lib/types';

// The viewer's own current web-player track is rendered from local state (always
// un-redacted and instant), while every other session comes from the server feed.
// jellyfin/navidrome/plex web playback is surfaced by the server-side poll, so the
// local overlay also covers it and we de-dupe the echo below.
const LOCAL_SOURCES = new Set(['jellyfin', 'navidrome', 'plex', 'local', 'youtube']);

function buildLocalSession(): NowPlayingSession | null {
	const state = playerStore.playbackState;
	if (state === 'idle' || state === 'error') return null;
	const np = playerStore.nowPlaying;
	if (!np || !np.trackName) return null;

	const src = np.sourceType;
	if (!LOCAL_SOURCES.has(src)) return null;

	return {
		id: `local-${src}-${np.trackSourceId ?? np.albumId}`,
		user_name: authStore.user?.display_name ?? '',
		track_name: np.trackName ?? '',
		artist_name: np.artistName,
		album_name: np.albumName,
		cover_url: np.coverUrl ?? '',
		device_name: '',
		is_paused: state === 'paused' || state === 'buffering' || state === 'loading',
		source: src,
		progress_ms: playerStore.progress * 1000,
		duration_ms: playerStore.duration * 1000,
		_isLocal: true
	};
}

function createMergedStore() {
	const mergedSessions = $derived.by(() =>
		mergeNowPlayingSessions(buildLocalSession(), nowPlayingStore.sessions, authStore.user?.id)
	);

	const activeSessions = $derived(mergedSessions.filter((s) => !s.is_paused));
	const primarySession = $derived(activeSessions[0] ?? mergedSessions[0] ?? null);

	function isSourcePlaying(src: string): boolean {
		return mergedSessions.some((s) => s.source === src && !s.is_paused);
	}

	function sourceHasSessions(src: string): boolean {
		return mergedSessions.some((s) => s.source === src);
	}

	function sessionsForSource(src: string): NowPlayingSession[] {
		return mergedSessions.filter((s) => s.source === src);
	}

	return {
		get sessions() {
			return mergedSessions;
		},
		get activeSessions() {
			return activeSessions;
		},
		get primarySession() {
			return primarySession;
		},
		isSourcePlaying,
		sourceHasSessions,
		sessionsForSource,
		start: nowPlayingStore.start,
		stop: nowPlayingStore.stop,
		refresh: nowPlayingStore.refresh
	};
}

export const nowPlayingMerged = createMergedStore();
