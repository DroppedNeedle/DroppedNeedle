import { expect, it } from 'vitest';
import { dedupeByMbid, toMusicBrainzAlbums, type SearchResultItemV3 } from './SearchV3Adapters';

const row = (over: Partial<SearchResultItemV3>): SearchResultItemV3 => ({
	kind: 'album',
	title: 'Visions',
	in_library: false,
	requested: false,
	score: 100,
	...over
});

it('drops rows without an MBID', () => {
	const albums = toMusicBrainzAlbums([
		row({ musicbrainz_id: 'rg-1', id: 'local-1', in_library: true }),
		row({ id: 'local-only' }),
		row({ musicbrainz_id: 'rg-2' })
	]);

	expect(albums.map((album) => [album.musicbrainz_id, album.local_id])).toEqual([
		['rg-1', 'local-1'],
		['rg-2', null]
	]);
});

it('keeps one row per release group across pages, preferring the library copy', () => {
	const rows = dedupeByMbid([
		{ musicbrainz_id: 'RG-1', local_id: null },
		{ musicbrainz_id: 'rg-1', local_id: 'local-1' },
		{ musicbrainz_id: 'rg-2', local_id: null }
	]);

	expect(rows).toEqual([
		{ musicbrainz_id: 'rg-1', local_id: 'local-1' },
		{ musicbrainz_id: 'rg-2', local_id: null }
	]);
});
