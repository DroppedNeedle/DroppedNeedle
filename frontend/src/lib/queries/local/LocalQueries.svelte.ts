import { createQuery, queryOptions, keepPreviousData } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { API, CACHE_TTL } from '$lib/constants';
import { api } from '$lib/api/client';
import type {
	CrateResponse,
	DecadesResponse,
	LocalAlbumSummary,
	LocalPaginatedResponse,
	LocalSearchResponse,
	LocalStorageStats
} from '$lib/types';

export const LOCAL_KEYS = {
	root: ['local'] as const,
	albums: (sort: string, decade: number | null) => ['local', 'albums', sort, decade] as const,
	recent: () => ['local', 'recent'] as const,
	suggestions: (decade: number | null) => ['local', 'suggestions', decade] as const,
	search: (q: string) => ['local', 'search', q] as const,
	decades: () => ['local', 'decades'] as const,
	stats: () => ['local', 'stats'] as const
};

export const getLocalRecentQuery = () =>
	createQuery(() =>
		queryOptions({
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.recent(),
			queryFn: ({ signal }) => api.global.get<LocalAlbumSummary[]>(API.local.recent(), { signal })
		})
	);

export interface LocalAlbumsParams {
	sort: string;
	decade?: number;
	limit?: number;
}

export const getLocalAlbumsQuery = (getParams: Getter<LocalAlbumsParams>) =>
	createQuery(() => {
		const { sort, decade, limit = 24 } = getParams();
		return queryOptions({
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.albums(sort, decade ?? null),
			queryFn: ({ signal }) =>
				api.global.get<LocalPaginatedResponse>(
					API.local.albums(limit, 0, sort, undefined, 'asc', decade),
					{ signal }
				)
		});
	});

export const getLocalSuggestionsQuery = (getDecade: Getter<number | undefined>) =>
	createQuery(() => {
		const decade = getDecade();
		return queryOptions({
			// crate should feel alive, never serve stale
			staleTime: 0,
			gcTime: 0,
			queryKey: LOCAL_KEYS.suggestions(decade ?? null),
			queryFn: ({ signal }) =>
				api.global.get<CrateResponse>(API.local.suggestions(16, decade), { signal })
		});
	});

// keepPreviousData avoids flashing empty while a new term is in flight
export const getLocalSearchQuery = (getTerm: Getter<string>) =>
	createQuery(() => {
		const term = getTerm().trim();
		return queryOptions({
			enabled: term.length >= 2,
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			placeholderData: keepPreviousData,
			queryKey: LOCAL_KEYS.search(term),
			queryFn: ({ signal }) =>
				api.global.get<LocalSearchResponse>(API.local.search(term), { signal })
		});
	});

export const getLocalDecadesQuery = () =>
	createQuery(() =>
		queryOptions({
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.decades(),
			queryFn: ({ signal }) => api.global.get<DecadesResponse>(API.local.decades(), { signal })
		})
	);

// enabled getter prevents firing when local files aren't configured, so it never hits a missing endpoint
export const getLocalStatsQuery = (enabled: () => boolean = () => true) =>
	createQuery(() =>
		queryOptions({
			enabled: enabled(),
			staleTime: CACHE_TTL.LIBRARY_NATIVE,
			queryKey: LOCAL_KEYS.stats(),
			queryFn: ({ signal }) => api.global.get<LocalStorageStats>(API.local.stats(), { signal })
		})
	);
