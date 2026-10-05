import { describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

import type { Album, Artist, LibraryAlbumSummary, LibraryArtistSummary } from '$lib/types';

import { SearchQueryKeyFactory } from './SearchQueryKeyFactory';
import { mergeSearchAlbums, mergeSearchArtists } from './SearchQueries.svelte';

describe('Search queries', () => {
	it('dimensions every provider key by user and MusicBrainz source identity', () => {
		expect(SearchQueryKeyFactory.artists('user-a', 'Muse', 6)).toEqual([
			'search',
			'user-a',
			{ user_id: 'user-a', source_mode: 'brainzmash', source_id: '', generation: 0 },
			'artists',
			'muse',
			6
		]);
		expect(SearchQueryKeyFactory.artists('user-b', 'Muse', 6)).not.toEqual(
			SearchQueryKeyFactory.artists('user-a', 'Muse', 6)
		);
		expect(SearchQueryKeyFactory.suggestions('user-a', ' Muse ', 5)).toEqual([
			'search',
			'user-a',
			{ user_id: 'user-a', source_mode: 'brainzmash', source_id: '', generation: 0 },
			'suggestions',
			'muse',
			5
		]);
		expect(SearchQueryKeyFactory.localArtists('user-a', ' Muse ', 24)).toEqual([
			'search',
			'user-a',
			'local-artists',
			'muse',
			24
		]);
		expect(SearchQueryKeyFactory.localAlbums('user-a', 'Muse', 5)).not.toEqual(
			SearchQueryKeyFactory.localAlbums('user-a', 'Muse', 24)
		);
	});

	it('coalesces provider-identical local artists and keeps local navigation identity', () => {
		const remote: Artist[] = [
			{
				title: 'Provider Name',
				musicbrainz_id: 'artist-mbid',
				in_library: false,
				thumb_url: 'https://images.test/artist.jpg'
			}
		];
		const local: LibraryArtistSummary[] = [
			{
				id: 'local-artist-id',
				name: 'Library Name',
				musicbrainz_artist_id: 'artist-mbid',
				artist_identity_state: 'musicbrainz_linked',
				album_count: 3,
				track_count: 20,
				appearance_release_count: 0,
				appearance_track_count: 0,
				library_relationship: 'album_artist',
				date_added: null,
				row_revision: 1
			}
		];

		expect(mergeSearchArtists(local, remote)).toEqual([
			{
				title: 'Library Name',
				musicbrainz_id: 'artist-mbid',
				in_library: true,
				thumb_url: 'https://images.test/artist.jpg',
				local_id: 'local-artist-id',
				release_group_count: 3
			}
		]);
	});

	it('places local-only albums first without name-based identity merging', () => {
		const remote: Album[] = [
			{
				title: 'Same Spelling',
				artist: 'Artist',
				year: 2020,
				musicbrainz_id: 'remote-rg',
				in_library: false
			}
		];
		const local: LibraryAlbumSummary[] = [
			{
				id: 'local-album-id',
				title: 'Same Spelling',
				artist_name: 'Artist',
				artist_id: 'local-artist-id',
				musicbrainz_release_group_id: null,
				musicbrainz_release_id: null,
				musicbrainz_artist_id: null,
				album_identity_state: 'local_only',
				track_count: 9,
				total_duration_seconds: 100,
				total_size_bytes: 1,
				format: 'FLAC',
				year: 2020,
				is_compilation: false,
				release_type: 'album',
				cover_available: true,
				date_added: null,
				sort_name: null,
				original_release_date: null,
				contribution_id: null,
				contribution_state: null
			}
		];

		const merged = mergeSearchAlbums(local, remote);

		expect(merged).toHaveLength(2);
		expect(merged[0]).toMatchObject({
			musicbrainz_id: 'local-album-id',
			local_id: 'local-album-id',
			in_library: true
		});
		expect(merged[1].musicbrainz_id).toBe('remote-rg');
	});
});
