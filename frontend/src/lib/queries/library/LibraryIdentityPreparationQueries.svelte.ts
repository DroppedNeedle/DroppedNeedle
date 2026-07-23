import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import type {
	IdentityPreparationEstimateResponse,
	OperationListResponse,
	RepairFindingListResponse
} from './LibraryOperationsTypes';

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
			queryFn: ({ pageParam, signal }) =>
				api.global.get<OperationListResponse>(API.library.identityPreparations(5, pageParam), {
					signal
				}),
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
			queryFn: ({ signal }) =>
				api.global.get<IdentityPreparationEstimateResponse>(
					API.library.identityPreparationEstimate(rootIds),
					{ signal }
				),
			staleTime: 10_000
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
			queryFn: ({ pageParam, signal }) =>
				api.global.get<RepairFindingListResponse>(
					API.library.identityPreparationFindings(jobId ?? '', 100, pageParam, findingCategory),
					{ signal }
				),
			getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined
		};
	});
