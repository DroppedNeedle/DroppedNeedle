import { api, ApiError } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type {
	JellyfinAlbumMatch,
	LocalAlbumMatch,
	LocalTrackInfo,
	NavidromeAlbumMatch,
	PlexAlbumMatch
} from '$lib/types';
import { toJellyfinTrack, toNavidromeTrack, toPlexTrack } from './remotes/remoteAdapters';
import { remoteApi } from './remotes/remoteApi';
import type { components } from '$lib/api/v3/openapi';

// "Which copy of this MusicBrainz album does each source hold": the local
// catalog and each remote source answer through v3, adapted into the match
// shapes the album page, album cards and artist radio read.

const localMatchUrl = (mbid: string) =>
	v3('/api/v3/local-library/albums/match/{mbid}', { path: { mbid }, query: { limit: 500 } });

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
		// `match_album` is shared with the remotes route in the contract, so
		// the typed client cannot infer this read; it names the page type.
		const page = await api.global.get<components['schemas']['TrackPage']>(localMatchUrl(mbid), {
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

export async function fetchJellyfinAlbumMatch(
	mbid: string,
	signal?: AbortSignal
): Promise<JellyfinAlbumMatch> {
	const match = await remoteApi.match('jellyfin', mbid, signal);
	return {
		found: match.found,
		jellyfin_album_id: match.remote_album_id ?? null,
		tracks: match.tracks.map(toJellyfinTrack)
	};
}

export async function fetchNavidromeAlbumMatch(
	mbid: string,
	signal?: AbortSignal
): Promise<NavidromeAlbumMatch> {
	const match = await remoteApi.match('navidrome', mbid, signal);
	return {
		found: match.found,
		navidrome_album_id: match.remote_album_id ?? null,
		tracks: match.tracks.map(toNavidromeTrack)
	};
}

export async function fetchPlexAlbumMatch(
	mbid: string,
	signal?: AbortSignal
): Promise<PlexAlbumMatch> {
	const match = await remoteApi.match('plex', mbid, signal);
	return {
		found: match.found,
		plex_album_id: match.remote_album_id ?? null,
		tracks: match.tracks.map(toPlexTrack)
	};
}
