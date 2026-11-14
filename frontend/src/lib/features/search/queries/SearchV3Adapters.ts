import type { components } from '$lib/api/v3/openapi';
import { normalizeDiscNumber } from '$lib/features/player/playback/queueHelpers';
import type { QueueItem } from '$lib/features/player/playback/types';
import type { Album } from '$lib/features/album/queries/types';
import type { Artist, SuggestResult } from '$lib/features/search/queries/types';
import { getApiUrl } from '$lib/api/api-utils';
import { libraryCoverPath } from '$lib/features/library/queries/LocalV3Adapters';

export type SearchResultItemV3 = components['schemas']['SearchResultItem'];
export type SearchTrackDetailV3 = components['schemas']['SearchTrackDetail'];
export type SearchRemoteTrackDetailV3 = components['schemas']['SearchRemoteTrackDetail'];
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
		local_id: row.id ?? null,
		disambiguation: row.disambiguation ?? null,
		type_info: row.type_info ?? null
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
		local_id: row.id ?? null,
		art_missing: row.art_missing
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
export type SearchLibrarySong = SearchResultItemV3 & { id: string; track: SearchTrackDetailV3 };

/** A song the library lacks: a track row with its recording and what requesting it needs. */
export type SearchRemoteSong = SearchResultItemV3 & {
	musicbrainz_id: string;
	remote_track: SearchRemoteTrackDetailV3;
};

export type SearchSong = SearchLibrarySong | SearchRemoteSong;

export function isLibrarySong(song: SearchSong): song is SearchLibrarySong {
	return !!song.id && !!song.track;
}

/** The song hits of a tracks bucket, in rank order: the library's, playable, and MusicBrainz's, requestable. */
export function toSearchSongs(rows: SearchResultItemV3[]): SearchSong[] {
	return rows.filter(
		(row): row is SearchSong =>
			row.kind === 'track' &&
			((!!row.id && !!row.track) || (!!row.musicbrainz_id && !!row.remote_track))
	);
}

/** The key a song list renders a song under: its library id, else its recording. */
export function searchSongKey(song: SearchSong): string {
	return song.id ?? song.musicbrainz_id ?? '';
}

/** The queue item that plays one library song. */
export function searchSongQueueItem(song: SearchLibrarySong): QueueItem {
	const detail = song.track;
	const cover = libraryCoverPath(
		detail.album_id,
		detail.release_group_mbid,
		song.art_missing || (!detail.release_group_mbid && !detail.cover_available)
	);
	return {
		trackSourceId: song.id,
		trackName: song.title,
		artistName: song.artist ?? '',
		trackNumber: detail.track_number,
		discNumber: normalizeDiscNumber(detail.disc_number),
		albumId: detail.album_id,
		albumName: detail.album_title,
		coverUrl: cover ? getApiUrl(cover) : null,
		coverRemoteUrl: null,
		sourceType: 'local',
		artistId: detail.artist_id ?? undefined,
		format: detail.format.toLowerCase(),
		availableSources: ['local'],
		duration: detail.duration_seconds ?? undefined
	};
}
