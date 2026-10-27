import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api, ApiError } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import { CACHE_TTL } from '$lib/constants';
import type { LocalAlbumMatch, LocalTrackInfo } from '$lib/types';
import type { components } from '$lib/api/v3/openapi';
import { remoteApi } from './remotes/remoteApi';
import type { RemoteMatchHints } from './remotes/endpoints';
import type { RemoteMatch, RemoteSource } from './remotes/types';
import { userIdSegment } from './userKeySegment';

// "Which copy of this MusicBrainz album does each source hold": the local
// catalog and each remote source answer through v3. The local answer is
// adapted into the match shape the album page, album cards and artist radio
// read; a remote answer is the contract's match view.

const localMatchUrl = (mbid: string) =>
	v3('/api/v3/library/albums/match/{mbid}', { path: { mbid }, query: { limit: 500 } });

function toLocalTrack(track: components['schemas']['TrackView']): LocalTrackInfo {
	return {
		track_file_id: track.id,
		title: track.title,
		track_number: track.track_number,
		disc_number: track.disc_number,
		duration_seconds: track.duration_seconds ?? null,
		size_bytes: track.file_size_bytes,
		format: track.format,
		bitrate: track.bit_rate ?? null
	};
}

function mostCommonFormat(tracks: LocalTrackInfo[]): string | null {
	const counts = new Map<string, number>();
	for (const track of tracks) counts.set(track.format, (counts.get(track.format) ?? 0) + 1);
	let best: string | null = null;
	for (const [format, count] of counts) if (!best || count > (counts.get(best) ?? 0)) best = format;
	return best;
}

export async function fetchLocalAlbumMatch(
	mbid: string,
	signal?: AbortSignal
): Promise<LocalAlbumMatch> {
	try {
		const page = await api.global.v3.GET(localMatchUrl(mbid), {
			signal
		});
		const tracks = page.items.map(toLocalTrack);
		return {
			found: tracks.length > 0,
			musicbrainz_id: mbid,
			tracks,
			total_size_bytes: tracks.reduce((sum, track) => sum + track.size_bytes, 0),
			primary_format: mostCommonFormat(tracks)
		};
	} catch (error) {
		if (error instanceof ApiError && error.status === 404) {
			return { found: false, musicbrainz_id: mbid, tracks: [], total_size_bytes: 0 };
		}
		throw error;
	}
}

/** A remote source's copy of the album. Jellyfin matches on the MusicBrainz id alone. */
export function fetchRemoteAlbumMatch(
	source: RemoteSource,
	mbid: string,
	hints: RemoteMatchHints,
	signal?: AbortSignal
): Promise<RemoteMatch> {
	return remoteApi.match(source, mbid, source === 'jellyfin' ? {} : hints, signal);
}

type UserId = string | null | undefined;

// Keyed by user (the remote sources are the caller's own connections), then
// album, so one album's sources or all of a user's refresh together.
export const AlbumMatchKeyFactory = {
	prefix: ['album-match'] as const,
	user: (userId: UserId) => [...AlbumMatchKeyFactory.prefix, userIdSegment(userId)] as const,
	album: (userId: UserId, mbid: string) => [...AlbumMatchKeyFactory.user(userId), mbid] as const,
	local: (userId: UserId, mbid: string) =>
		[...AlbumMatchKeyFactory.album(userId, mbid), 'local'] as const,
	remote: (userId: UserId, source: RemoteSource, mbid: string, hints: RemoteMatchHints) =>
		[
			...AlbumMatchKeyFactory.album(userId, mbid),
			source,
			source === 'jellyfin' ? '' : (hints.title ?? ''),
			source === 'jellyfin' ? '' : (hints.artist ?? '')
		] as const
};

export const getLocalAlbumMatchQuery = (
	getUserId: Getter<UserId>,
	getMbid: Getter<string>,
	getEnabled: Getter<boolean>
) =>
	createQuery(() => {
		const mbid = getMbid();
		return {
			queryKey: AlbumMatchKeyFactory.local(getUserId(), mbid),
			enabled: getEnabled() && !!mbid,
			staleTime: CACHE_TTL.ALBUM_DETAIL_SOURCE_MATCH,
			queryFn: ({ signal }: { signal: AbortSignal }) => fetchLocalAlbumMatch(mbid, signal)
		};
	});

export const getRemoteAlbumMatchQuery = (
	getUserId: Getter<UserId>,
	getParams: Getter<{
		source: RemoteSource;
		mbid: string;
		hints: RemoteMatchHints;
		enabled: boolean;
	}>
) =>
	createQuery(() => {
		const { source, mbid, hints, enabled } = getParams();
		return {
			queryKey: AlbumMatchKeyFactory.remote(getUserId(), source, mbid, hints),
			enabled: enabled && !!mbid,
			staleTime: CACHE_TTL.ALBUM_DETAIL_SOURCE_MATCH,
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				fetchRemoteAlbumMatch(source, mbid, hints, signal)
		};
	});
