import type { components } from '$lib/api/v3/openapi';
import { gatewayStreamUrl } from '$lib/player/playbackGateway';
import { normalizeDiscNumber } from '$lib/player/queueHelpers';
import type { QueueItem } from '$lib/player/types';
import type { Album, Artist, SearchRemoteStatus, SuggestResult } from '$lib/types';
import { getCoverUrl } from '$lib/utils/errorHandling';

export type SearchResultItemV3 = components['schemas']['SearchResultItem'];
export type SearchTrackDetailV3 = components['schemas']['SearchTrackDetail'];
export type SearchRemoteStatusV3 = components['schemas']['SearchRemoteStatus'];
export type SuggestResultV3 = components['schemas']['SuggestResult'];

// Transitional bridge: the search routes read through the v3 hooks while the
// shared cards below still take v1 Artist/Album props. Rows without a
// provider id fall back to the local id, the same pattern as the v1 merge
// functions; both cards already treat local_id === musicbrainz_id as
// local-only (no download button, no enrichment request). MusicBrainz rows
// the library does not hold have no local id; every row has one id or the
// other.
function rowKey(row: { id?: string | null; musicbrainz_id?: string | null }): string {
	return row.musicbrainz_id ?? row.id ?? '';
}

/** A library row with no MusicBrainz id: rowKey gave it its local id. */
export function isLocalOnlyRow(row: { local_id?: string | null; musicbrainz_id: string }): boolean {
	return !!row.local_id && row.local_id === row.musicbrainz_id;
}

/** The MusicBrainz id an image may be requested by. Never a local id. */
export function imageMbid(row: { local_id?: string | null; musicbrainz_id: string }): string {
	return isLocalOnlyRow(row) ? '' : row.musicbrainz_id;
}

export function toV1Artist(row: SearchResultItemV3): Artist {
	return {
		title: row.title,
		musicbrainz_id: rowKey(row),
		in_library: row.in_library,
		score: row.score,
		local_id: row.id ?? null
	};
}

export function toV1Album(row: SearchResultItemV3): Album {
	return {
		title: row.title,
		artist: row.artist ?? null,
		year: row.year ?? null,
		musicbrainz_id: rowKey(row),
		in_library: row.in_library,
		requested: row.requested,
		score: row.score,
		local_id: row.id ?? null
	};
}

/**
 * One row per MusicBrainz id across pages. The server sends one row per
 * release group within a response, but MusicBrainz paging can shift between
 * requests, so a grid built from several pages can see a release group
 * twice. The first position wins; the row with a library id is kept, so it
 * can link to the copy.
 */
export function dedupeByMbid<T extends { musicbrainz_id: string; local_id?: string | null }>(
	rows: T[]
): T[] {
	const out: T[] = [];
	const positions = new Map<string, number>();
	for (const row of rows) {
		const key = row.musicbrainz_id.toLowerCase();
		const at = positions.get(key);
		if (at === undefined) {
			positions.set(key, out.length);
			out.push(row);
		} else if (!out[at].local_id && row.local_id) {
			out[at] = row;
		}
	}
	return out;
}

/**
 * Album picks that must name a MusicBrainz release group (drop-import match,
 * YouTube link). Library-only rows carry just a local id, so they are left
 * out rather than offered under a local id.
 */
export function toMusicBrainzAlbums(rows: SearchResultItemV3[]): Album[] {
	return rows.filter((row) => !!row.musicbrainz_id).map(toV1Album);
}

// The v3 contract mirrors the v1 status values exactly, so statuses pass
// straight into getSearchStatusNotice and the stale-time helpers. Typed as
// a function so the build fails if the two unions ever drift apart.
export function toSearchRemoteStatus(status: SearchRemoteStatusV3): SearchRemoteStatus {
	return status;
}

// Typeahead rows keep the v1 shape the shell handlers route on, while the
// data comes from the v3 suggest endpoint (the local catalog and
// MusicBrainz, merged across buckets, best first, with library and request
// flags). Track rows have no dropdown route, so they drop here; the caller
// over-fetches to keep the list full.
export function toSuggestResultsV1(rows: SuggestResultV3[]): SuggestResult[] {
	const out: SuggestResult[] = [];
	for (const row of rows) {
		if (row.kind !== 'artist' && row.kind !== 'album') continue;
		out.push({
			type: row.kind,
			title: row.title,
			artist: row.artist ?? null,
			musicbrainz_id: rowKey(row),
			in_library: row.in_library,
			requested: row.requested,
			score: row.score,
			local_id: row.id ?? null
		});
	}
	return out;
}

/** A library song hit: a track row with its id and library details. */
export type SearchSong = SearchResultItemV3 & { id: string; track: SearchTrackDetailV3 };

/** The playable song hits of a tracks bucket, in rank order. */
export function toSearchSongs(rows: SearchResultItemV3[]): SearchSong[] {
	return rows.filter((row): row is SearchSong => row.kind === 'track' && !!row.id && !!row.track);
}

/** The queue item that plays one library song. */
export function searchSongQueueItem(song: SearchSong): QueueItem {
	const detail = song.track;
	return {
		trackSourceId: song.id,
		trackName: song.title,
		artistName: song.artist ?? '',
		trackNumber: detail.track_number,
		discNumber: normalizeDiscNumber(detail.disc_number),
		albumId: detail.album_id,
		albumName: detail.album_title,
		coverUrl: getCoverUrl(null, detail.album_id),
		coverRemoteUrl: null,
		sourceType: 'local',
		artistId: detail.artist_id ?? undefined,
		streamUrl: gatewayStreamUrl('local', song.id),
		format: detail.format.toLowerCase(),
		availableSources: ['local'],
		duration: detail.duration_seconds ?? undefined
	};
}
