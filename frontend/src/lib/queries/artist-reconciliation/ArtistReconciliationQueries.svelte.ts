import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import { ArtistReconciliationQueryKeyFactory } from './ArtistReconciliationQueryKeyFactory';
import { ARTIST_RECONCILIATION_ENDPOINTS } from './endpoints';
import type {
	ArtistDuplicateGroupListResponse,
	ArtistDuplicateGroupParams
} from './ArtistReconciliationTypes';

const GROUP_PAGE_SIZE = 24;

export const getArtistReconciliationProgressQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled(),
		queryKey: ArtistReconciliationQueryKeyFactory.progress(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(ARTIST_RECONCILIATION_ENDPOINTS.progress(), { signal }),
		refetchInterval: (query) =>
			query.state.data && ['queued', 'running', 'pausing'].includes(query.state.data.state)
				? 2000
				: false
	}));

export const getArtistDuplicateGroupsQuery = (getParams: Getter<ArtistDuplicateGroupParams>) =>
	createInfiniteQuery(() => {
		const params = getParams();
		return {
			queryKey: ArtistReconciliationQueryKeyFactory.groups(authStore.user?.id, params),
			initialPageParam: undefined as string | undefined,
			queryFn: ({ pageParam, signal }) =>
				api.global.v3.GET(
					ARTIST_RECONCILIATION_ENDPOINTS.groups({
						limit: GROUP_PAGE_SIZE,
						cursor: pageParam,
						state: params.state,
						search: params.search?.trim() || undefined
					}),
					{ signal }
				),
			getNextPageParam: (lastPage: ArtistDuplicateGroupListResponse) =>
				lastPage.next_cursor ?? undefined
		};
	});

export const getArtistDuplicateGroupQuery = (getGroupId: Getter<string | null>) =>
	createQuery(() => {
		const groupId = getGroupId();
		return {
			enabled: Boolean(groupId),
			queryKey: ArtistReconciliationQueryKeyFactory.group(authStore.user?.id, groupId ?? ''),
			queryFn: ({ signal }) =>
				api.global.v3.GET(ARTIST_RECONCILIATION_ENDPOINTS.group(groupId ?? ''), { signal })
		};
	});
