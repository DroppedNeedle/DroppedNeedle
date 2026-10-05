import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { v3 } from '$lib/api/v3/endpoint';

export type GatewaySource = 'local' | 'jellyfin' | 'navidrome' | 'plex';
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

// Gateway URLs, built through the typed registry (player call sites import
// from here so a route rename touches this file only), except `stream`:
// the backend route is an Axum wildcard (/stream/{source}/{*key} in
// server/src/stream/routes.rs) while the spec spells a single-segment
// {key}, because OpenAPI cannot express the wildcard. Plex part keys carry
// their own slashes, so the key always travels raw and the typed builder's
// path encoding would corrupt it. That one row stays a hand-built string by
// necessity and sits outside the contract-coverage gate.
export const GATEWAY_ENDPOINTS = {
	stream: (source: GatewaySource, key: string, hints: StreamHints = {}) => {
		const search = new URLSearchParams();
		if (hints.format) search.set('format', hints.format);
		if (hints.max_bitrate !== undefined) search.set('max_bitrate', String(hints.max_bitrate));
		if (hints.estimate_content_length !== undefined) {
			search.set('estimate_content_length', String(hints.estimate_content_length));
		}
		const query = search.toString();
		// Plex part keys carry their own slashes and the route is a wildcard,
		// so the key always travels raw, never encoded.
		return `/api/v3/stream/${source}/${key}${query ? `?${query}` : ''}`;
	},
	playbackStart: () => v3('/api/v3/playback/start'),
	playbackProgress: () => v3('/api/v3/playback/progress'),
	playbackStop: () => v3('/api/v3/playback/stop'),
	scrobbleNowPlaying: () => v3('/api/v3/scrobble/now-playing'),
	scrobbleSubmit: () => v3('/api/v3/scrobble/submit'),
	nowPlaying: () => v3('/api/v3/now-playing')
} as const;

// One gateway URL for every source: local file ids, Jellyfin/Navidrome item
// ids, and raw Plex part keys all stream from the same route shape.
export function gatewayStreamUrl(
	source: GatewaySource,
	key: string | number,
	hints: StreamHints = {}
): string {
	return GATEWAY_ENDPOINTS.stream(source, String(key), hints);
}

// Plain reporters for player internals (progress loops, track-change stops,
// beforeunload): contexts where a TanStack mutation's bookkeeping does not
// fit. This file is the only playback surface: every leg reports through it.
export async function startPlaybackSession(
	request: PlaybackStartRequest
): Promise<PlaybackStartResponse> {
	return api.global.v3.POST(GATEWAY_ENDPOINTS.playbackStart(), request);
}

export async function reportPlaybackProgress(request: PlaybackProgressRequest): Promise<void> {
	await api.global.v3.POST(GATEWAY_ENDPOINTS.playbackProgress(), request);
}

export async function stopPlaybackSession(request: PlaybackStopRequest): Promise<void> {
	await api.global.v3.POST(GATEWAY_ENDPOINTS.playbackStop(), request);
}

export async function sendScrobbleNowPlaying(
	request: ScrobbleNowPlayingRequest
): Promise<ScrobbleResponse> {
	return api.global.v3.POST(GATEWAY_ENDPOINTS.scrobbleNowPlaying(), request);
}

export async function submitScrobble(request: ScrobbleSubmitRequest): Promise<ScrobbleResponse> {
	return api.global.v3.POST(GATEWAY_ENDPOINTS.scrobbleSubmit(), request);
}

export async function fetchNowPlayingSnapshot(signal?: AbortSignal): Promise<NowPlayingSnapshot> {
	return api.global.v3.GET(GATEWAY_ENDPOINTS.nowPlaying(), { signal });
}
