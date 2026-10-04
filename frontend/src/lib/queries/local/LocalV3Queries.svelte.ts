import { createQuery, keepPreviousData, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { LOCAL_V3_KEYS, type LocalV3AlbumsParams, type LocalV3PageParams } from './LocalV3Keys';
import { LocalV3Api } from './LocalV3Api';

export type AlbumCardPageV3 = components['schemas']['AlbumCardPage'];
export type AlbumCardV3 = components['schemas']['AlbumCard'];
export type DecadesResponseV3 = components['schemas']['DecadesResponse'];
export type LocalSearchResultsV3 = components['schemas']['SearchResults'];
export type SuggestionsResponseV3 = components['schemas']['SuggestionsResponse'];
export type SuggestionTrackV3 = components['schemas']['SuggestionTrack'];
export type LocalStatsV3 = components['schemas']['StatsView'];
export type LocalAlbumMatchV3 = components['schemas']['TrackPage'];
export type LocalTrackV3 = components['schemas']['TrackView'];

export const getLocalRecentV3Query = (getLimit: Getter<number | null> = () => null) =>
	createQuery(() => {
		const limit = getLimit();
		return queryOptions({
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_V3_KEYS.recent(limit),
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
			queryKey: LOCAL_V3_KEYS.albums(params),
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
			queryKey: LOCAL_V3_KEYS.suggestions(limit, decade),
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
			queryKey: LOCAL_V3_KEYS.search(term, limit),
			queryFn: ({ signal }) => api.global.v3.GET(LocalV3Api.search(term, limit), { signal })
		});
	});

export const getLocalDecadesV3Query = () =>
	createQuery(() =>
		queryOptions({
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_V3_KEYS.decades(),
			queryFn: ({ signal }) => api.global.v3.GET(LocalV3Api.decades(), { signal })
		})
	);

export const getLocalStatsV3Query = () =>
	createQuery(() =>
		queryOptions({
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_V3_KEYS.stats(),
			queryFn: ({ signal }) => api.global.v3.GET(LocalV3Api.stats(), { signal })
		})
	);

export const getLocalAlbumMatchV3Query = (
	getMbid: Getter<string>,
	getPage: Getter<LocalV3PageParams> = () => ({})
) =>
	createQuery(() => {
		const mbid = getMbid();
		const page = getPage();
		return queryOptions({
			enabled: mbid.length > 0,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_V3_KEYS.albumMatch(mbid, page),
			queryFn: ({ signal }) => api.global.v3.GET(LocalV3Api.albumMatch(mbid, page), { signal })
		});
	});
