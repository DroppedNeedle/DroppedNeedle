import { describe, expect, it } from 'vitest';
import type { AlbumView } from '$lib/types';
import { fileOwnedAlbums } from './artistLibrarySections';

function album(id: string, rg: string | null, extra: Partial<AlbumView> = {}) {
	return {
		id,
		title: id,
		release_group_mbid: rg,
		is_compilation: false,
		year: null,
		...extra
	} as AlbumView;
}

const titles = (list: { album: AlbumView }[]) => list.map((filed) => filed.album.title);

describe('fileOwnedAlbums', () => {
	it('files every owned album, whatever its type, newest first and undated last', () => {
		const sections = fileOwnedAlbums(
			[
				album('studio', 'RG-1'),
				album('best-of', 'rg-2'),
				album('single', 'rg-3'),
				album('live', 'rg-4'),
				album('untyped', 'rg-5'),
				album('tagged compilation', null, { is_compilation: true }),
				album('no mbid', null)
			],
			[
				{ id: 'rg-1', primary_type: 'Album', secondary_types: [] },
				{ id: 'rg-2', primary_type: 'Album', secondary_types: ['Compilation'] },
				{ id: 'rg-3', primary_type: 'Single', secondary_types: [] },
				{ id: 'rg-4', primary_type: 'Album', secondary_types: ['Live'] }
			]
		);
		expect(titles(sections.albums)).toEqual(['studio', 'untyped', 'no mbid']);
		expect(titles(sections.epsAndSingles)).toEqual(['single']);
		expect(titles(sections.other)).toEqual(['best-of', 'live', 'tagged compilation']);
		expect(sections.other.map((filed) => filed.label)).toEqual([
			'Compilation',
			'Live',
			'Compilation'
		]);
		const dated = fileOwnedAlbums(
			[
				album('old', null, { year: 1990 }),
				album('undated', null),
				album('new', null, { year: 2020 })
			],
			[]
		);
		expect(titles(dated.albums)).toEqual(['new', 'old', 'undated']);
	});
});
