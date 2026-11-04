import { unifiedSearchQuery } from '$lib/features/search/queries/unifiedSearchQuery';
import { pageQuery } from '$lib/features/library/queries/listQueries';
import { albumsQuery } from '$lib/features/library/queries/listQueries';
import { v3 } from '$lib/api/v3/endpoint';
import { createQuery, keepPreviousData, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import type { SearchV3Limits } from '$lib/features/search/queries/SearchQueryKeyFactory';
import { LOCAL_KEYS, type LocalV3AlbumsParams } from './LocalV3Keys';
import { readNow } from '$lib/queries/readNow';
import { STALE_LIVE } from '$lib/queries/staleTime';
import { authStore } from '$lib/stores/authStore.svelte';
import { PERSIST } from '$lib/queries/persistedCache';
import type { AlbumView } from './types';

export type SuggestionTrackV3 = components['schemas']['SuggestionTrack'];

// The card fields of a library album. The caller's favorite flag is left
// out, so the shared local keys never cache one user's flags for the next.
export type AlbumCardV3 = Pick<
	AlbumView,
	| 'id'
	| 'title'
	| 'artist_name'
	| 'artist_mbid'
	| 'release_group_mbid'
	| 'year'
	| 'track_count'
	| 'total_size_bytes'
	| 'format'
	| 'cover_available'
	| 'art_missing'
	| 'date_added'
>;

function toCard(album: AlbumView): AlbumCardV3 {
	return {
		id: album.id,
		title: album.title,
		artist_name: album.artist_name,
		artist_mbid: album.artist_mbid,
		release_group_mbid: album.release_group_mbid,
		year: album.year,
		track_count: album.track_count,
		total_size_bytes: album.total_size_bytes,
		format: album.format,
		cover_available: album.cover_available,
		art_missing: album.art_missing,
		date_added: album.date_added
	};
}

export const getLocalRecentV3Query = (getLimit: Getter<number> = () => 20) =>
	createQuery(() => {
		const limit = getLimit();
		return queryOptions({
			meta: PERSIST,
			queryKey: LOCAL_KEYS.recent(limit),
			queryFn: async ({ signal }) => {
				const page = await api.v3.GET(v3('/api/v3/library/recently-added', { query: { limit } }), {
					signal
				});
				return page.items.map(toCard);
			}
		});
	});

export const getLocalAlbumsV3Query = (
	getParams: Getter<LocalV3AlbumsParams>,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => {
		const params = getParams();
		return queryOptions({
			enabled: getEnabled(),
			meta: PERSIST,
			queryKey: LOCAL_KEYS.albums(params),
			queryFn: async ({ signal }) => {
				const page = await api.v3.GET(
					v3('/api/v3/library/albums', {
						query: albumsQuery(params, null)
					}),
					{ signal }
				);
				return { ...page, items: page.items.map(toCard) };
			}
		});
	});

export const getLocalSuggestionsV3Query = (
	getDecade: Getter<number | undefined>,
	getLimit: Getter<number> = () => 16
) =>
	createQuery(() => {
		const decade = getDecade() ?? null;
		const limit = getLimit();
		return queryOptions({
			// crate should feel alive, never serve stale
			staleTime: STALE_LIVE,
			gcTime: 0,
			queryKey: LOCAL_KEYS.suggestions(limit, decade),
			queryFn: ({ signal }) =>
				api.v3.GET(
					v3('/api/v3/library/suggestions', {
						query: decade === null ? { limit } : { limit, decade }
					}),
					{
						signal
					}
				)
		});
	});

const LIBRARY_SEARCH_LIMITS: SearchV3Limits = { artists: 0, albums: 20, tracks: 20 };

// Unified search over the library alone, albums and songs. It reads no
// per-user data, so it keeps a shared local key that library changes
// invalidate. keepPreviousData avoids flashing empty while a new term is in
// flight.
export const getLibrarySearchV3Query = (getTerm: Getter<string>) =>
	createQuery(() => {
		const term = getTerm().trim();
		return queryOptions({
			enabled: term.length >= 2,
			placeholderData: keepPreviousData,
			queryKey: LOCAL_KEYS.search(term),
			queryFn: ({ signal }) =>
				api.v3.GET(
					v3('/api/v3/search', {
						query: unifiedSearchQuery(term, LIBRARY_SEARCH_LIMITS, ['albums', 'tracks'], 'library')
					}),
					{ signal }
				)
		});
	});

export const getLocalDecadesV3Query = () =>
	createQuery(() =>
		queryOptions({
			meta: PERSIST,
			queryKey: LOCAL_KEYS.decades(),
			queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/library/decades'), { signal })
		})
	);

// The enabled getter keeps the read off when local files are not set up.
export const getLocalStatsV3Query = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() =>
		queryOptions({
			enabled: getEnabled(),
			meta: PERSIST,
			queryKey: LOCAL_KEYS.stats(authStore.user?.id),
			queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/library/stats'), { signal })
		})
	);

/** Every track of one library album, by its local id, to play or queue it whole. */
export const fetchLocalAlbumTracksV3 = (albumId: string) =>
	readNow(LOCAL_KEYS.albumMatch(albumId, { limit: 1000 }), (signal) =>
		api.v3.GET(
			v3('/api/v3/library/albums/match/{mbid}', {
				path: { mbid: albumId },
				query: pageQuery({ limit: 1000 })
			}),
			{ signal }
		)
	);

/** A fresh batch of suggestions for play all and shuffle. */
export const fetchLocalSuggestionBatchV3 = () =>
	readNow(LOCAL_KEYS.suggestions(40, null), (signal) =>
		api.v3.GET(
			v3('/api/v3/library/suggestions', {
				query: { limit: 40 }
			}),
			{ signal }
		)
	);
