import { getApiUrl } from '$lib/api/api-utils';
import { gatewayStreamUrl } from '$lib/player/playbackGateway';
import type { QueueItem, SourceType } from '$lib/player/types';
import type { CrateReason, CrateTrack, LocalAlbumSummary } from '$lib/types';
import type { AlbumCardV3, LocalTrackV3, SuggestionTrackV3 } from './LocalV3Queries.svelte';

// Adapters from v3 local-library views to the listening room's track and
// album shapes. The crate, search card, and turntable trade these shapes
// through callbacks and drag payloads, so the page adapts at the query
// edge instead of retyping every component.

const KNOWN_REASONS: readonly CrateReason[] = ['recent', 'rediscover', 'surprise', 'same_era'];

function toCrateReason(reason: string): CrateReason {
	return (KNOWN_REASONS as readonly string[]).includes(reason)
		? (reason as CrateReason)
		: 'surprise';
}

// v3 cover URL for one release-group mbid, or null when the album is
// unidentified. Callers wrap it with getApiUrl at render time.
export function localCoverUrl(
	releaseGroupMbid: string | null | undefined,
	size: 250 | 500 | 1200 = 250
): string | null {
	if (!releaseGroupMbid) return null;
	return `/api/v3/covers/release-group/${releaseGroupMbid}?size=${size}`;
}

// Absolute cover URL for a crate track: the adapted v3 path first, then
// the mbid-derived fallback, else null for the placeholder art.
export function crateCoverUrl(track: Pick<CrateTrack, 'cover_url' | 'album_mbid'>): string | null {
	if (track.cover_url) return getApiUrl(track.cover_url);
	const fallback = localCoverUrl(track.album_mbid);
	return fallback ? getApiUrl(fallback) : null;
}

// Absolute cover URL for an album summary, same fallback order as tracks.
export function albumCoverUrl(
	album: Pick<LocalAlbumSummary, 'cover_url' | 'musicbrainz_id'>
): string | null {
	if (album.cover_url) return getApiUrl(album.cover_url);
	const fallback = localCoverUrl(album.musicbrainz_id);
	return fallback ? getApiUrl(fallback) : null;
}

function unixToIso(seconds: number | null | undefined): string | null {
	if (!seconds) return null;
	return new Date(seconds * 1000).toISOString();
}

export function albumCardToSummary(card: AlbumCardV3): LocalAlbumSummary {
	return {
		musicbrainz_id: card.release_group_mbid ?? card.id,
		name: card.title,
		artist_name: card.artist_name,
		artist_mbid: card.artist_mbid,
		year: card.year,
		track_count: card.track_count,
		total_size_bytes: card.total_size_bytes,
		primary_format: card.primary_format,
		cover_url: localCoverUrl(card.release_group_mbid),
		date_added: unixToIso(card.date_added)
	};
}

// Suggestion tracks carry the owning album's local id but no mbid, so the
// caller passes the mbid from its loaded album cards (null when unknown).
export function suggestionToCrateTrack(
	suggestion: SuggestionTrackV3,
	albumMbid: string | null
): CrateTrack {
	return {
		track_file_id: suggestion.track_id,
		title: suggestion.title,
		album_name: suggestion.album_title,
		artist_name: suggestion.artist_name,
		album_mbid: albumMbid,
		cover_url: localCoverUrl(albumMbid),
		format: suggestion.format,
		year: suggestion.year,
		duration_seconds: suggestion.duration_seconds,
		reason: toCrateReason(suggestion.reason)
	};
}

// Queue item for one matched album track. The v3 track id is the stream
// key, matching the discovery queue builder for local tracks.
export function matchTrackToQueueItem(
	track: LocalTrackV3,
	coverUrl: string | null,
	albumMbid: string
): QueueItem {
	return {
		trackSourceId: track.id,
		trackName: track.title,
		artistName: track.artist_name,
		trackNumber: track.track_number,
		discNumber: track.disc_number,
		albumId: albumMbid,
		albumName: track.album_title,
		coverUrl,
		coverRemoteUrl: null,
		sourceType: 'local',
		artistId: track.artist_id ?? undefined,
		streamUrl: gatewayStreamUrl('local', track.id),
		format: track.format.toLowerCase(),
		availableSources: ['local'] as SourceType[],
		duration: track.duration_seconds ?? undefined
	};
}
