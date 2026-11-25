import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

export type NewRelease = components['schemas']['NewRelease'];
export type NewReleaseSheet = components['schemas']['NewReleaseSheet'];

/** Releases asked for per page of the whole list. */
export const NEW_RELEASES_PAGE = 50;

/** The caller's new releases from their artists, newest first, a page at a time. */
export const getNewReleasesQuery = () =>
	createInfiniteQuery(() => ({
		enabled: Boolean(authStore.user?.id),
		queryKey: DiscoverQueryKeyFactory.v3.newReleases(authStore.user?.id),
		initialPageParam: null as string | null,
		queryFn: ({ pageParam, signal }) =>
			api.v3.GET(
				v3('/api/v3/discover/new-releases', {
					query: { limit: NEW_RELEASES_PAGE, ...(pageParam ? { cursor: pageParam } : {}) }
				}),
				{ signal }
			),
		getNextPageParam: (lastPage: components['schemas']['Page_NewRelease']) =>
			lastPage.next_cursor ?? undefined
	}));
