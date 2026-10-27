import { createQuery, keepPreviousData, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { LibraryV3Api } from '../library/LibraryV3Api';
import { SearchV3Api } from '../search/SearchV3Api';
import type { SearchV3Limits } from '../search/SearchQueryKeyFactory';
import { LOCAL_KEYS, type LocalV3AlbumsParams } from './LocalV3Keys';

export type SuggestionTrackV3 = components['schemas']['SuggestionTrack'];
export type LocalTrackV3 = components['schemas']['TrackView'];
type AlbumViewV3 = components['schemas']['AlbumView'];

// The card fields of a library album. The caller's favorite flag is left
// out, so the shared local keys never cache one user's flags for the next.
export type AlbumCardV3 = Pick<
	AlbumViewV3,
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
	| 'date_added'
>;

function toCard(album: AlbumViewV3): AlbumCardV3 {
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
		date_added: album.date_added
	};
}

export const getLocalRecentV3Query = (getLimit: Getter<number> = () => 20) =>
	createQuery(() => {
		const limit = getLimit();
		return queryOptions({
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.recent(limit),
			queryFn: async ({ signal }) => {
				const page = await api.global.v3.GET(LibraryV3Api.recentlyAdded(limit), { signal });
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
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.albums(params),
			queryFn: async ({ signal }) => {
				const page = await api.global.v3.GET(LibraryV3Api.albums(params), { signal });
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
			staleTime: 0,
			gcTime: 0,
			queryKey: LOCAL_KEYS.suggestions(limit, decade),
			queryFn: ({ signal }) =>
				api.global.v3.GET(LibraryV3Api.suggestions(limit, decade), {
					signal
				})
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
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			placeholderData: keepPreviousData,
			queryKey: LOCAL_KEYS.search(term),
			queryFn: ({ signal }) =>
				api.global.v3.GET(
					SearchV3Api.unified(term, LIBRARY_SEARCH_LIMITS, ['albums', 'tracks'], 'library'),
					{ signal }
				)
		});
	});

export const getLocalDecadesV3Query = () =>
	createQuery(() =>
		queryOptions({
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.decades(),
			queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.decades(), { signal })
		})
	);

// The enabled getter keeps the read off when local files are not set up.
export const getLocalStatsV3Query = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() =>
		queryOptions({
			enabled: getEnabled(),
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.stats(),
			queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.stats(), { signal })
		})
	);
