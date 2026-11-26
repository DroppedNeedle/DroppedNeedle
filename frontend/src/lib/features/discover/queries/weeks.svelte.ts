import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

export type PastWeeksPage = components['schemas']['PastWeeksPage'];
export type PastWeek = PastWeeksPage['items'][number];
export type PastPick = components['schemas']['PastPick'];
export type PickAnswer = components['schemas']['PickAnswer'];
export type AnswerCounts = components['schemas']['AnswerCounts'];
export type WeeksRecord = components['schemas']['WeeksRecord'];
export type FoundPick = components['schemas']['FoundPick'];

/** Weeks one page asks for. */
export const WEEKS_PAGE = 8;

/** What narrows the picks of past weeks; none for every pick. */
export interface WeeksFilter {
	state: PickAnswer | null;
	search: string;
}

/** The caller's past weeks, newest first, a page of weeks at a time, narrowed by `getFilter`. */
export const getPastWeeksQuery = (
	getFilter: Getter<WeeksFilter> = () => ({ state: null, search: '' }),
	getEnabled: Getter<boolean> = () => true
) =>
	createInfiniteQuery(() => {
		const { state, search } = getFilter();
		const q = search.trim();
		return {
			enabled: getEnabled() && Boolean(authStore.user?.id),
			queryKey: DiscoverQueryKeyFactory.v3.pastWeeksList(authStore.user?.id, state, q),
			initialPageParam: null as string | null,
			queryFn: ({ pageParam, signal }) =>
				api.v3.GET(
					v3('/api/v3/discover/weeks', {
						query: {
							limit: WEEKS_PAGE,
							...(pageParam ? { cursor: pageParam } : {}),
							...(state ? { state } : {}),
							...(q ? { q } : {})
						}
					}),
					{ signal }
				),
			getNextPageParam: (lastPage: PastWeeksPage) => lastPage.next_cursor ?? undefined
		};
	});

/**
 * The albums the caller kept from their sets. One read serves every album
 * page, which looks its album up here and asks nothing more.
 */
export const getFoundOnDiscoverQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		queryKey: DiscoverQueryKeyFactory.v3.found(authStore.user?.id),
		staleTime: 10 * 60_000,
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/discover/found'), { signal })
	}));
