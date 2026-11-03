import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import type {
	IdentityPreparationEstimateResponse,
	RepairFindingListResponse
} from '$lib/features/library-management/queries/LibraryOperationsTypes';
import { STALE_LIVE } from '$lib/queries/staleTime';

export const getLibraryIdentityPreparationsQuery = (
	getUserId: Getter<string | undefined>,
	enabled: Getter<boolean>
) =>
	createInfiniteQuery(() => {
		const userId = getUserId();
		return {
			enabled: enabled() && Boolean(userId),
			queryKey: LibraryQueryKeyFactory.identityPreparations(userId, undefined),
			initialPageParam: undefined as string | undefined,
			queryFn: async ({ pageParam, signal }) =>
				await api.v3.GET(
					v3('/api/v3/library/management/identity-preparations', {
						query: { limit: 5, ...(pageParam ? { cursor: pageParam } : {}) }
					}),
					{ signal }
				),
			getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
			refetchInterval: (query) =>
				query.state.data?.pages.some((page) =>
					page.items.some((item) => ['queued', 'running', 'paused'].includes(item.state))
				)
					? 2_500
					: false
		};
	});

export const getLibraryIdentityPreparationEstimateQuery = (
	getUserId: Getter<string | undefined>,
	getRootIds: Getter<string[]>,
	enabled: Getter<boolean>
) =>
	createQuery(() => {
		const userId = getUserId();
		const rootIds = getRootIds();
		return {
			enabled: enabled() && Boolean(userId),
			queryKey: LibraryQueryKeyFactory.identityPreparationEstimate(userId, rootIds),
			queryFn: async ({ signal }): Promise<IdentityPreparationEstimateResponse> =>
				api.v3.GET(
					v3('/api/v3/library/management/identity-preparations/estimate', {
						query: rootIds.length ? { root_id: rootIds } : {}
					}),
					{ signal }
				),
			staleTime: STALE_LIVE
		};
	});

export const getLibraryIdentityPreparationFindingsQuery = (
	getUserId: Getter<string | undefined>,
	getJobId: Getter<string | null>,
	getFindingCategory: Getter<string>
) =>
	createInfiniteQuery(() => {
		const userId = getUserId();
		const jobId = getJobId();
		const findingCategory = getFindingCategory();
		return {
			enabled: Boolean(userId && jobId),
			queryKey: LibraryQueryKeyFactory.identityPreparationFindings(
				userId,
				jobId ?? '',
				findingCategory,
				undefined
			),
			initialPageParam: undefined as string | undefined,
			queryFn: async ({ pageParam, signal }) =>
				(await api.v3.GET(
					v3('/api/v3/library/management/identity-preparations/{job_id}/findings', {
						path: { job_id: jobId ?? '' },
						query: {
							limit: 100,
							...(pageParam ? { cursor: pageParam } : {}),
							...(findingCategory ? { finding_category: findingCategory } : {})
						}
					}),
					{ signal }
				)) as RepairFindingListResponse,
			getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined
		};
	});
