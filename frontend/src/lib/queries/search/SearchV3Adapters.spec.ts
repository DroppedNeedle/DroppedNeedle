import { expect, it } from 'vitest';
import { toMusicBrainzAlbums, type SearchResultItemV3 } from './SearchV3Adapters';

const row = (over: Partial<SearchResultItemV3>): SearchResultItemV3 => ({
	kind: 'album',
	title: 'Visions',
	in_library: false,
	requested: false,
	score: 100,
	...over
});

it('drops rows without an MBID and keeps one row per release group', () => {
	const albums = toMusicBrainzAlbums([
		row({ musicbrainz_id: 'RG-1' }),
		row({ id: 'local-only' }),
		row({ musicbrainz_id: 'rg-1', id: 'local-1', in_library: true }),
		row({ musicbrainz_id: 'rg-2' })
	]);

	expect(albums.map((album) => [album.musicbrainz_id, album.local_id])).toEqual([
		['rg-1', 'local-1'],
		['rg-2', null]
	]);
});
