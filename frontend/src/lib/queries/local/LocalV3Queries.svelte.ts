import { createQuery, keepPreviousData, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { LOCAL_KEYS, type LocalV3AlbumsParams } from './LocalV3Keys';
import { LocalV3Api } from './LocalV3Api';

export type AlbumCardV3 = components['schemas']['AlbumCard'];
export type SuggestionTrackV3 = components['schemas']['SuggestionTrack'];
export type LocalTrackV3 = components['schemas']['TrackView'];

export const getLocalRecentV3Query = (getLimit: Getter<number | null> = () => null) =>
	createQuery(() => {
		const limit = getLimit();
		return queryOptions({
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.recent(limit),
			queryFn: ({ signal }) => api.global.v3.GET(LocalV3Api.recent(limit), { signal })
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
			queryFn: ({ signal }) => api.global.v3.GET(LocalV3Api.albums(params), { signal })
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
				api.global.v3.GET(LocalV3Api.suggestions(limit, decade), {
					signal
				})
		});
	});

// keepPreviousData avoids flashing empty while a new term is in flight
export const getLocalSearchV3Query = (
	getTerm: Getter<string>,
	getLimit: Getter<number | null> = () => null
) =>
	createQuery(() => {
		const term = getTerm().trim();
		const limit = getLimit();
		return queryOptions({
			enabled: term.length >= 2,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			placeholderData: keepPreviousData,
			queryKey: LOCAL_KEYS.search(term, limit),
			queryFn: ({ signal }) => api.global.v3.GET(LocalV3Api.search(term, limit), { signal })
		});
	});

export const getLocalDecadesV3Query = () =>
	createQuery(() =>
		queryOptions({
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.decades(),
			queryFn: ({ signal }) => api.global.v3.GET(LocalV3Api.decades(), { signal })
		})
	);

// The enabled getter keeps the read off when local files are not set up.
export const getLocalStatsV3Query = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() =>
		queryOptions({
			enabled: getEnabled(),
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.stats(),
			queryFn: ({ signal }) => api.global.v3.GET(LocalV3Api.stats(), { signal })
		})
	);
