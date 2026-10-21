import { playerStore } from '$lib/stores/player.svelte';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import type { SourceType } from '$lib/player/types';
import {
	sendScrobbleNowPlaying,
	submitScrobble,
	type ScrobbleNowPlayingRequest,
	type ScrobbleSubmitRequest
} from '$lib/player/playbackGateway';
import { SCROBBLE_PREFERENCES_ENDPOINTS } from '$lib/queries/scrobble-preferences/endpoints';
import {
	makeTrackKey,
	shouldAccumulate,
	isLoopReset,
	shouldSendNowPlaying,
	shouldScrobble,
	formatServiceTooltip
} from '$lib/stores/scrobbleHelpers';
import { registerUserSessionReset } from '$lib/utils/userSessionCleanup';

export type NowPlayingSubmission = ScrobbleNowPlayingRequest;
export type ScrobbleSubmission = ScrobbleSubmitRequest;
export type ScrobbleResponse = components['schemas']['ScrobbleResponse'];

// The manager only reads the three forwarding flags out of the full
// per-user scrobble preferences.
export type ScrobbleSettings = Pick<
	components['schemas']['ScrobblePreferences'],
	'scrobble_to_lastfm' | 'scrobble_to_listenbrainz' | 'navidrome_handles_external_scrobbles'
>;

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

/** The finished play, as the player knew it. */
export interface CompletedPlay {
	artistName: string;
	trackName: string;
	albumName: string;
	sourceType: SourceType;
	trackSourceId?: string;
	recordingMbid?: string;
}

/**
 * A play from the library names its track, which the server files it
 * under. A play through any other source sends the recording that source
 * gave for the file, when it gave one.
 */
export function makeScrobbleSubmission(
	play: CompletedPlay,
	durationMs: number,
	timestamp: number
): ScrobbleSubmission {
	const body: ScrobbleSubmission = {
		track_name: play.trackName,
		artist_name: play.artistName,
		album_name: play.albumName,
		duration_ms: durationMs,
		timestamp,
		source: play.sourceType
	};
	if (play.sourceType === 'local') {
		if (play.trackSourceId) body.track_id = play.trackSourceId;
	} else if (play.recordingMbid) {
		body.mbid = play.recordingMbid;
	}
	return body;
}

function createScrobbleManager() {
	let status = $state<ScrobbleStatus>('idle');
	// A user is signed in: plays are tracked and recorded in their history.
	let active = $state(false);
	// Forwarding to Last.fm or ListenBrainz is on.
	let enabled = $state(false);
	let lastServiceDetail = $state<Record<string, { queued: boolean }> | null>(null);
	let track: TrackState | null = null;
	let settingsCache: ScrobbleSettings | null = null;
	let lastSettingsFetch = 0;
	let progressInterval: ReturnType<typeof setInterval> | null = null;

	async function loadSettings(): Promise<ScrobbleSettings | null> {
		const now = Date.now();
		if (settingsCache && now - lastSettingsFetch < 60_000) return settingsCache;
		try {
			settingsCache = await api.global.v3.GET(SCROBBLE_PREFERENCES_ENDPOINTS.get());
			lastSettingsFetch = now;
			return settingsCache;
		} catch {
			return settingsCache;
		}
	}

	async function init(): Promise<void> {
		active = true;
		const settings = await loadSettings();
		if (!active) return;
		enabled = !!(settings?.scrobble_to_lastfm || settings?.scrobble_to_listenbrainz);
	}

	function stopInterval(): void {
		if (progressInterval) {
			clearInterval(progressInterval);
			progressInterval = null;
		}
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
			await sendScrobbleNowPlaying(body);
		} catch {
			status = 'error';
		}
	}

	async function sendScrobble(
		play: CompletedPlay,
		durationMs: number,
		timestamp: number
	): Promise<void> {
		try {
			const body = makeScrobbleSubmission(play, durationMs, timestamp);
			const data = await submitScrobble(body);
			lastServiceDetail = Object.fromEntries(
				Object.entries(data.services ?? {}).map(([k, v]) => [k, { queued: v.queued }])
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

			if (!active) {
				track = null;
				if (status !== 'idle') status = 'idle';
				stopInterval();
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

				if (enabled && shouldSendNowPlaying(track.accumulatedMs, track.nowPlayingSent)) {
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
						{
							artistName: currentNp.artistName,
							trackName: currentNp.trackName,
							albumName: currentNp.albumName,
							sourceType: currentNp.sourceType,
							trackSourceId: currentNp.trackSourceId,
							recordingMbid: currentNp.recordingMbid
						},
						t.durationMs,
						t.startedAt
					).catch(() => {
						t.scrobbled = false;
					});
				}
			}, 1000);

			return stopInterval;
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
			// tooltip never reflect the wrong user. A play started before
			// sign-out is dropped, never submitted under the next session.
			active = false;
			track = null;
			stopInterval();
			enabled = false;
			status = 'idle';
			lastServiceDetail = null;
			settingsCache = null;
			lastSettingsFetch = 0;
		}
	};
}

export const scrobbleManager = createScrobbleManager();
registerUserSessionReset(() => scrobbleManager.reset());

export { formatServiceTooltip };
