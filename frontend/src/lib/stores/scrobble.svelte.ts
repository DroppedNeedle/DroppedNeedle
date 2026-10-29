import { playerStore } from '$lib/stores/player.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { queryClient } from '$lib/queries/QueryClient';
import { getScrobblePreferencesQueryOptions } from '$lib/queries/scrobble-preferences/ScrobblePreferencesQuery.svelte';
import type { components } from '$lib/api/v3/openapi';
import type { SourceType } from '$lib/player/types';
import {
	sendScrobbleNowPlaying,
	submitScrobble,
	type ScrobbleNowPlayingRequest,
	type ScrobbleSubmitRequest
} from '$lib/player/playbackGateway';
import {
	makeTrackKey,
	shouldAccumulate,
	isLoopReset,
	shouldSendNowPlaying,
	shouldScrobble,
	scrobbleRetryDelayMs,
	formatServiceTooltip
} from '$lib/stores/scrobbleHelpers';
import { registerUserSessionReset } from '$lib/utils/userSessionCleanup';

export type NowPlayingSubmission = ScrobbleNowPlayingRequest;
export type ScrobbleSubmission = ScrobbleSubmitRequest;
export type ScrobbleResponse = components['schemas']['ScrobbleResponse'];

type ScrobbleStatus = 'idle' | 'tracking' | 'scrobbled' | 'error';

interface TrackState {
	trackKey: string;
	accumulatedMs: number;
	lastProgressS: number;
	nowPlayingSent: boolean;
	scrobbled: boolean;
	startedAt: number;
	durationMs: number;
	submitFailures: number;
	nextSubmitAt: number;
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
	let progressInterval: ReturnType<typeof setInterval> | null = null;

	/** Reads the forwarding flag from the signed-in user's preferences query. */
	async function loadSettings(fresh: boolean): Promise<void> {
		active = true;
		const userId = authStore.user?.id;
		if (!userId) return;
		const options = getScrobblePreferencesQueryOptions(userId);
		let settings: components['schemas']['ScrobblePreferences'];
		try {
			settings = await queryClient.fetchQuery(fresh ? { ...options, staleTime: 0 } : options);
		} catch {
			return;
		}
		// A sign-out or another sign-in while this was in flight leaves the answer unused.
		if (!active || authStore.user?.id !== userId) return;
		enabled = !!(settings.scrobble_to_lastfm || settings.scrobble_to_listenbrainz);
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
					durationMs,
					submitFailures: 0,
					nextSubmitAt: 0
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
					track.submitFailures = 0;
					track.nextSubmitAt = 0;
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

				if (
					Date.now() >= track.nextSubmitAt &&
					shouldScrobble(track.accumulatedMs, track.durationMs, track.scrobbled)
				) {
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
						// Retried after a growing pause, a few times per play; then it stays failed.
						t.submitFailures += 1;
						const delay = scrobbleRetryDelayMs(t.submitFailures);
						if (delay === null) return;
						t.nextSubmitAt = Date.now() + delay;
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
		init: () => loadSettings(false),
		refreshSettings: () => loadSettings(true),
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
		}
	};
}

export const scrobbleManager = createScrobbleManager();
registerUserSessionReset(() => scrobbleManager.reset());

export { formatServiceTooltip };
