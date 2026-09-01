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

import { CACHE_TTL, API } from '$lib/constants';
import { api } from '$lib/api/client';
import type { Album, Artist, LibraryAlbumSummary, LibraryArtistSummary } from '$lib/types';

import { SearchQueryKeyFactory } from './SearchQueryKeyFactory';
import {
	getLocalAlbumSearchQueryOptions,
	getLocalArtistSearchQueryOptions,
	getRemoteArtistSearchQueryOptions,
	mergeSearchAlbums,
	mergeSearchArtists,
	REMOTE_ARTIST_PAGE_SIZE,
	successfulSearchStaleTime,
	successfulSuggestStaleTime,
	SEARCH_FAILURE_STALE_TIME_MS
} from './SearchQueries.svelte';

describe('Search queries', () => {
	it('uses the bucket-width remote artist profile', async () => {
		expect(REMOTE_ARTIST_PAGE_SIZE).toBe(24);

		const remote = getRemoteArtistSearchQueryOptions(' Muse ');
		const options = remote as unknown as {
			queryKey: unknown;
			queryFn: (context: { signal: AbortSignal }) => Promise<unknown>;
		};
		expect(options.queryKey).toEqual(SearchQueryKeyFactory.artists('user-1', 'muse', 24));
		const signal = new AbortController().signal;
		await options.queryFn({ signal });
		expect(api.global.get).toHaveBeenCalledWith(API.search.artists('Muse', 24), { signal });
	});
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

describe('B6 search failure-floor staleness', () => {
	it('successful search keeps the full SEARCH window', () => {
		expect(successfulSearchStaleTime({ state: { data: { status: 'ok' } } })).toBe(CACHE_TTL.SEARCH);
	});

	it('failed search holds the failure floor instead of collapsing to 0', () => {
		expect(successfulSearchStaleTime({ state: { data: { status: 'error' } } })).toBe(
			SEARCH_FAILURE_STALE_TIME_MS
		);
		expect(SEARCH_FAILURE_STALE_TIME_MS).toBe(60_000);
		expect(successfulSearchStaleTime({ state: { data: undefined } })).toBe(
			SEARCH_FAILURE_STALE_TIME_MS
		);
	});

	it('suggest resolver mirrors the same floor on remote_status', () => {
		expect(successfulSuggestStaleTime({ state: { data: { remote_status: 'ok' } } })).toBe(
			CACHE_TTL.SEARCH
		);
		expect(successfulSuggestStaleTime({ state: { data: { remote_status: 'error' } } })).toBe(
			SEARCH_FAILURE_STALE_TIME_MS
		);
		expect(successfulSuggestStaleTime({ state: {} })).toBe(SEARCH_FAILURE_STALE_TIME_MS);
	});
});

describe('B7 local search prefetch factories', () => {
	it('produce the same keys/staleTime as the mounted queries, gated like them', () => {
		const artists = getLocalArtistSearchQueryOptions('Muse');
		expect(artists.queryKey).toEqual(SearchQueryKeyFactory.localArtists('user-1', 'Muse', 24));
		expect(artists.staleTime).toBe(CACHE_TTL.SEARCH);
		expect(artists.enabled).toBe(true);

		const albums = getLocalAlbumSearchQueryOptions('Muse');
		expect(albums.queryKey).toEqual(SearchQueryKeyFactory.localAlbums('user-1', 'Muse', 24));
		expect(albums.staleTime).toBe(CACHE_TTL.SEARCH);
		expect(albums.enabled).toBe(true);
	});

	it('short queries are gated off exactly like the mounted queries', () => {
		expect(getLocalArtistSearchQueryOptions('M').enabled).toBe(false);
		expect(getLocalAlbumSearchQueryOptions(' M ').enabled).toBe(false);
	});
});
