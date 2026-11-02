import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { streamPath } from '$lib/api/stream';
import { v3 } from '$lib/api/v3/endpoint';

export type GatewaySource = 'local' | 'jellyfin' | 'navidrome' | 'plex';
/** A media server whose plays the server reports on to it. */
export type RemoteSource = Exclude<GatewaySource, 'local'>;
export type PlaybackStartRequest = components['schemas']['PlaybackStartRequest'];
export type PlaybackStartResponse = components['schemas']['PlaybackStartResponse'];
export type PlaybackProgressRequest = components['schemas']['PlaybackProgressRequest'];
export type PlaybackStopRequest = components['schemas']['PlaybackStopRequest'];
export type ScrobbleNowPlayingRequest = components['schemas']['ScrobbleNowPlayingRequest'];
export type ScrobbleSubmitRequest = components['schemas']['ScrobbleSubmitRequest'];
export type ScrobbleResponse = components['schemas']['ScrobbleResponse'];
export type ScrobbleServiceResult = components['schemas']['ServiceResult'];
export type NowPlayingHeartbeat = components['schemas']['NowPlayingHeartbeat'];
export type NowPlayingSnapshot = components['schemas']['NowPlayingSnapshot'];
export type NowPlayingEntry = components['schemas']['NowPlayingEntry'];

export interface StreamHints {
	format?: string;
	max_bitrate?: number;
	estimate_content_length?: boolean;
}

// One gateway URL for every source: local file ids, Jellyfin/Navidrome item
// ids, and raw Plex part keys all stream from the same route shape. The typed
// builder cannot carry the wildcard key (see streamPath).
export function gatewayStreamUrl(
	source: GatewaySource,
	key: string | number,
	hints: StreamHints = {}
): string {
	const search = new URLSearchParams();
	if (hints.format) search.set('format', hints.format);
	if (hints.max_bitrate !== undefined) search.set('max_bitrate', String(hints.max_bitrate));
	if (hints.estimate_content_length !== undefined) {
		search.set('estimate_content_length', String(hints.estimate_content_length));
	}
	return streamPath(source, String(key), search);
}

// Plain reporters for player internals (progress loops, track-change stops,
// beforeunload): contexts where a TanStack mutation's bookkeeping does not
// fit. This file is the only playback surface: every leg reports through it.
export async function startPlaybackSession(
	request: PlaybackStartRequest
): Promise<PlaybackStartResponse> {
	return api.v3.POST(v3('/api/v3/playback/start'), request);
}

export async function reportPlaybackProgress(request: PlaybackProgressRequest): Promise<void> {
	await api.v3.POST(v3('/api/v3/playback/progress'), request);
}

export async function stopPlaybackSession(request: PlaybackStopRequest): Promise<void> {
	await api.v3.POST(v3('/api/v3/playback/stop'), request);
}

export async function sendScrobbleNowPlaying(
	request: ScrobbleNowPlayingRequest
): Promise<ScrobbleResponse> {
	return api.v3.POST(v3('/api/v3/scrobble/now-playing'), request);
}

export async function submitScrobble(request: ScrobbleSubmitRequest): Promise<ScrobbleResponse> {
	return api.v3.POST(v3('/api/v3/scrobble/submit'), request);
}

export async function fetchNowPlayingSnapshot(signal?: AbortSignal): Promise<NowPlayingSnapshot> {
	return api.v3.GET(v3('/api/v3/now-playing'), { signal });
}
