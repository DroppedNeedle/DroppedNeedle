import { playerStore } from '$lib/stores/player.svelte';
import type {
	NowPlayingSubmission,
	ScrobbleSubmission,
	ScrobbleSettings,
	ScrobbleResponse
} from '$lib/types';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import type { SourceType } from '$lib/player/types';
import {
	makeTrackKey,
	shouldAccumulate,
	isLoopReset,
	shouldSendNowPlaying,
	shouldScrobble,
	formatServiceTooltip
} from '$lib/stores/scrobbleHelpers';

type ScrobbleStatus = 'idle' | 'tracking' | 'scrobbled' | 'error';

interface TrackState {
	trackKey: string;
	accumulatedMs: number;
	lastProgressS: number;
	nowPlayingSent: boolean;
	scrobbled: boolean;
	startedAt: number;
	durationMs: number;
}

export function makeNowPlayingSubmission(
	artistName: string,
	trackName: string,
	albumName: string,
	durationMs: number,
	source: SourceType
): NowPlayingSubmission {
	return {
		track_name: trackName,
		artist_name: artistName,
		album_name: albumName,
		duration_ms: durationMs,
		source
	};
}

export function makeScrobbleSubmission(
	artistName: string,
	trackName: string,
	albumName: string,
	durationMs: number,
	timestamp: number,
	source: SourceType
): ScrobbleSubmission {
	return {
		track_name: trackName,
		artist_name: artistName,
		album_name: albumName,
		duration_ms: durationMs,
		timestamp,
		source
	};
}

function createScrobbleManager() {
	let status = $state<ScrobbleStatus>('idle');
	let enabled = $state(false);
	let lastServiceDetail = $state<Record<string, { success: boolean }> | null>(null);
	let track: TrackState | null = null;
	let settingsCache: ScrobbleSettings | null = null;
	let lastSettingsFetch = 0;
	let progressInterval: ReturnType<typeof setInterval> | null = null;

	async function loadSettings(): Promise<ScrobbleSettings | null> {
		const now = Date.now();
		if (settingsCache && now - lastSettingsFetch < 60_000) return settingsCache;
		try {
			settingsCache = await api.global.get<ScrobbleSettings>(API.me.scrobblePreferences());
			lastSettingsFetch = now;
			return settingsCache;
		} catch {
			return settingsCache;
		}
	}

	async function init(): Promise<void> {
		const settings = await loadSettings();
		enabled = !!(settings?.scrobble_to_lastfm || settings?.scrobble_to_listenbrainz);
	}

	async function sendNowPlaying(
		artistName: string,
		trackName: string,
		albumName: string,
		durationMs: number,
		source: SourceType
	): Promise<void> {
		try {
			const body = makeNowPlayingSubmission(artistName, trackName, albumName, durationMs, source);
			await api.global.post(API.scrobble.nowPlaying(), body);
		} catch {
			status = 'error';
		}
	}

	async function sendScrobble(
		artistName: string,
		trackName: string,
		albumName: string,
		durationMs: number,
		timestamp: number,
		source: SourceType
	): Promise<void> {
		try {
			const body = makeScrobbleSubmission(
				artistName,
				trackName,
				albumName,
				durationMs,
				timestamp,
				source
			);
			const data = await api.global.post<ScrobbleResponse>(API.scrobble.submit(), body);
			lastServiceDetail = Object.fromEntries(
				Object.entries(data.services).map(([k, v]) => [k, { success: v.success }])
			);
			if (data.accepted) {
				status = 'scrobbled';
			} else {
				status = 'error';
				throw new Error('Scrobble not accepted');
			}
		} catch (e) {
			status = 'error';
			lastServiceDetail = lastServiceDetail ?? null;
			throw e;
		}
	}

	$effect.root(() => {
		$effect(() => {
			const np = playerStore.nowPlaying;
			const isPlaying = playerStore.isPlaying;

			if (!enabled) {
				if (status !== 'idle') status = 'idle';
				if (progressInterval) {
					clearInterval(progressInterval);
					progressInterval = null;
				}
				return;
			}

			if (np?.isPreview) {
				// 30s radio previews are tastes, not listens: never scrobbled
				track = null;
				if (status !== 'idle') status = 'idle';
				if (progressInterval) {
					clearInterval(progressInterval);
					progressInterval = null;
				}
				return;
			}

			if (!np || !np.trackName || !np.artistName) {
				if (!np || !np.trackName) {
					track = null;
					if (status !== 'idle') {
						status = 'idle';
						lastServiceDetail = null;
					}
				}
				if (progressInterval) {
					clearInterval(progressInterval);
					progressInterval = null;
				}
				return;
			}

			if (!isPlaying) {
				if (progressInterval) {
					clearInterval(progressInterval);
					progressInterval = null;
				}
				return;
			}

			const currentKey = makeTrackKey(np.artistName, np.trackName);
			const durationMs = Math.round(playerStore.duration * 1000);

			if (!track || track.trackKey !== currentKey) {
				track = {
					trackKey: currentKey,
					accumulatedMs: 0,
					lastProgressS: playerStore.progress,
					nowPlayingSent: false,
					scrobbled: false,
					startedAt: Math.floor(Date.now() / 1000),
					durationMs
				};
				status = 'tracking';
				lastServiceDetail = null;
			}

			if (progressInterval) {
				clearInterval(progressInterval);
			}

			progressInterval = setInterval(() => {
				if (!track) return;
				const progressS = playerStore.progress;
				const currentDurationMs = Math.round(playerStore.duration * 1000);
				const currentNp = playerStore.nowPlaying;

				if (!currentNp || !currentNp.trackName || !playerStore.isPlaying) return;

				if (currentDurationMs > 0 && track.durationMs === 0) {
					track.durationMs = currentDurationMs;
				}

				const deltaS = progressS - track.lastProgressS;
				const prevProgressS = track.lastProgressS;
				track.lastProgressS = progressS;

				if (isLoopReset(prevProgressS, progressS, track.durationMs)) {
					track.accumulatedMs = 0;
					track.scrobbled = false;
					track.nowPlayingSent = false;
					track.startedAt = Math.floor(Date.now() / 1000);
					status = 'tracking';
					lastServiceDetail = null;
					return;
				}

				if (shouldAccumulate(deltaS)) {
					track.accumulatedMs += deltaS * 1000;
				}

				if (shouldSendNowPlaying(track.accumulatedMs, track.nowPlayingSent)) {
					track.nowPlayingSent = true;
					sendNowPlaying(
						currentNp.artistName,
						currentNp.trackName,
						currentNp.albumName,
						track.durationMs,
						currentNp.sourceType
					);
				}

				if (shouldScrobble(track.accumulatedMs, track.durationMs, track.scrobbled)) {
					track.scrobbled = true;
					const t = track;
					sendScrobble(
						currentNp.artistName,
						currentNp.trackName,
						currentNp.albumName,
						t.durationMs,
						t.startedAt,
						currentNp.sourceType
					).catch(() => {
						t.scrobbled = false;
					});
				}
			}, 1000);

			return () => {
				if (progressInterval) {
					clearInterval(progressInterval);
					progressInterval = null;
				}
			};
		});
	});

	return {
		get status() {
			return status;
		},
		get enabled() {
			return enabled;
		},
		get tooltip() {
			return formatServiceTooltip(status, lastServiceDetail);
		},
		init,
		async refreshSettings(): Promise<void> {
			settingsCache = null;
			await init();
		},
		reset(): void {
			// clear per-user scrobble state on user switch/logout so the client gate +
			// tooltip never reflect the wrong user (AMU-5)
			enabled = false;
			status = 'idle';
			lastServiceDetail = null;
			settingsCache = null;
			lastSettingsFetch = 0;
		}
	};
}

export const scrobbleManager = createScrobbleManager();

export { formatServiceTooltip };
