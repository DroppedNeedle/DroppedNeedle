import type { components } from '$lib/api/v3/openapi';
import type { Album, Artist, SearchRemoteStatus, SuggestResult } from '$lib/types';

export type SearchResultItemV3 = components['schemas']['SearchResultItem'];
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
