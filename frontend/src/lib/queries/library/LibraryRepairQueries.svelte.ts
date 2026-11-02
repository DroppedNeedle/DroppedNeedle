import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import type { RepairEstimateResponse, RepairFindingListResponse } from './LibraryOperationsTypes';

export const getLibraryRepairsQuery = (enabled: Getter<boolean> = () => true) =>
	createInfiniteQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.repairs(undefined),
		initialPageParam: undefined as string | undefined,
		queryFn: async ({ pageParam, signal }) =>
			await api.v3.GET(
				v3('/api/v3/library/identity-repairs', {
					query: { limit: 50, ...(pageParam ? { cursor: pageParam } : {}) }
				}),
				{ signal }
			),
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
				api.v3.GET(
					v3('/api/v3/library/identity-repairs/estimate', {
						query: rootIds.length ? { root_id: rootIds } : {}
					}),
					{ signal }
				)
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
					v3('/api/v3/library/identity-repairs/{job_id}/findings', {
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
