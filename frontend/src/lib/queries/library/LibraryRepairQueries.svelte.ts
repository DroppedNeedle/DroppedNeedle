import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import { toOperationList } from './libraryOperationAdapters';
import type { RepairEstimateResponse, RepairFindingListResponse } from './LibraryOperationsTypes';

export const getLibraryRepairsQuery = (enabled: Getter<boolean> = () => true) =>
	createInfiniteQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.repairs(undefined),
		initialPageParam: undefined as string | undefined,
		queryFn: async ({ pageParam, signal }) =>
			toOperationList(await api.v3.GET(LibraryV3Api.identityRepairs(50, pageParam), { signal })),
		getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined
	}));

export const getLibraryRepairEstimateQuery = (
	getRootIds: Getter<string[]>,
	enabled: Getter<boolean>
) =>
	createQuery(() => {
		const rootIds = getRootIds();
		return {
			enabled: enabled(),
			queryKey: LibraryQueryKeyFactory.repairEstimate(rootIds),
			queryFn: async ({ signal }): Promise<RepairEstimateResponse> =>
				api.v3.GET(LibraryV3Api.identityRepairEstimate(rootIds), { signal })
		};
	});

export const getLibraryRepairFindingsQuery = (
	getJobId: Getter<string | null>,
	getFindingCategory: Getter<string>
) =>
	createInfiniteQuery(() => {
		const jobId = getJobId();
		const findingCategory = getFindingCategory();
		return {
			enabled: Boolean(jobId),
			queryKey: LibraryQueryKeyFactory.repairFindings(jobId ?? '', findingCategory, undefined),
			initialPageParam: undefined as string | undefined,
			queryFn: async ({ pageParam, signal }) =>
				(await api.v3.GET(
					LibraryV3Api.identityRepairFindings(jobId ?? '', 100, pageParam, findingCategory),
					{ signal }
				)) as RepairFindingListResponse,
			getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined
		};
	});
