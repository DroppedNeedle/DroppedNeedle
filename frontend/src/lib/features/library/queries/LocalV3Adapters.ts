import { getApiUrl } from '$lib/api/api-utils';
import { albumArtworkPath, coverPath } from '$lib/api/covers';
import type { QueueItem, SourceType } from '$lib/features/player/playback/types';
import type { components } from '$lib/api/v3/openapi';
import type { CrateReason, CrateTrack, TrackView } from '$lib/features/library/queries/types';
import type { AlbumCardV3, SuggestionTrackV3 } from './LocalV3Queries.svelte';

type SearchItemV3 = components['schemas']['SearchResultItem'];

// What playing or queueing a library album needs. Every library album has a
// local id; only a linked one has a release group.
export type SpinAlbum = Pick<
	AlbumCardV3,
	'id' | 'title' | 'artist_name' | 'release_group_mbid' | 'year' | 'art_missing'
>;

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

// A library album's cover path: its release group's when linked, its own art
// otherwise, null when the server knows there is none.
export function libraryCoverPath(
	albumId: string,
	releaseGroupMbid: string | null | undefined,
	artMissing: boolean
): string | null {
	if (artMissing) return null;
	if (releaseGroupMbid) return coverPath('release-group', releaseGroupMbid, 250);
	return albumArtworkPath(albumId, undefined, 250);
}

export function crateCoverUrl(track: Pick<CrateTrack, 'cover_url'>): string | null {
	return track.cover_url ? getApiUrl(track.cover_url) : null;
}

export function albumCoverUrl(album: SpinAlbum): string | null {
	const path = libraryCoverPath(album.id, album.release_group_mbid, album.art_missing);
	return path ? getApiUrl(path) : null;
}

// A library album hit from search, or null for a hit without its library id.
export function searchAlbumToSpin(item: SearchItemV3): SpinAlbum | null {
	if (!item.id) return null;
	return {
		id: item.id,
		title: item.title,
		artist_name: item.artist ?? '',
		release_group_mbid: item.musicbrainz_id,
		year: item.year,
		art_missing: item.art_missing
	};
}

// A library song hit from search, or null for a hit without its library
// details.
export function searchTrackToCrateTrack(item: SearchItemV3): CrateTrack | null {
	const detail = item.track;
	if (!detail || !item.id) return null;
	const albumMbid = detail.release_group_mbid ?? null;
	return {
		track_file_id: item.id,
		title: item.title,
		album_name: detail.album_title,
		artist_name: item.artist ?? '',
		album_id: detail.album_id,
		album_mbid: albumMbid,
		cover_url: libraryCoverPath(
			detail.album_id,
			albumMbid,
			item.art_missing || (!albumMbid && !detail.cover_available)
		),
		format: detail.format,
		year: item.year,
		duration_seconds: detail.duration_seconds,
		reason: 'surprise'
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
		album_id: suggestion.album_id,
		album_mbid: albumMbid,
		cover_url: libraryCoverPath(
			suggestion.album_id,
			albumMbid,
			!albumMbid && !suggestion.cover_available
		),
		format: suggestion.format,
		year: suggestion.year,
		duration_seconds: suggestion.duration_seconds,
		reason: toCrateReason(suggestion.reason)
	};
}

// Queue item for one matched album track. The v3 track id is the stream
// key, matching the discovery queue builder for local tracks.
export function matchTrackToQueueItem(track: TrackView, coverUrl: string | null): QueueItem {
	return {
		trackSourceId: track.id,
		trackName: track.title,
		artistName: track.artist_name,
		trackNumber: track.track_number,
		discNumber: track.disc_number,
		albumId: track.release_group_mbid ?? track.album_id,
		albumName: track.album_title,
		coverUrl,
		coverRemoteUrl: null,
		sourceType: 'local',
		artistId: track.artist_id ?? undefined,
		format: track.format.toLowerCase(),
		availableSources: ['local'] as SourceType[],
		duration: track.duration_seconds ?? undefined
	};
}
