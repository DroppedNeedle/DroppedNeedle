import { createInfiniteQuery, createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import { toCurrentRuns, toRunDetail, toScanRun } from './libraryScanAdapters';
import type {
	OperationResponse,
	ScanEstimateResponse,
	ScanRunFailuresResponse,
	ScanRunHistoryResponse
} from './LibraryOperationsTypes';

// Scan runs read the v3 scan routes. Operation jobs have no v3 route yet
// (see the waiting-on-backend list in eslint.config.js).

const HISTORY_PAGE = 50;
const FAILURES_PAGE = 50;

export const getCurrentLibraryRunsQueryOptions = () =>
	queryOptions({
		queryKey: LibraryQueryKeyFactory.currentRuns(),
		queryFn: async ({ signal }) =>
			toCurrentRuns(await api.global.v3.GET(LibraryV3Api.currentScanRuns(), { signal })),
		staleTime: 2_000
	});

export const getCurrentLibraryRunsQuery = (enabled: Getter<boolean> = () => true) =>
	createQuery(() => ({ ...getCurrentLibraryRunsQueryOptions(), enabled: enabled() }));

export const getLibraryRunQuery = (getRunId: Getter<string | null>) =>
	createQuery(() => {
		const runId = getRunId();
		return {
			enabled: Boolean(runId),
			queryKey: LibraryQueryKeyFactory.run(runId ?? ''),
			queryFn: async ({ signal }) =>
				toRunDetail(await api.global.v3.GET(LibraryV3Api.scanRun(runId ?? ''), { signal }))
		};
	});

export const getLibraryRunHistoryQuery = (enabled: Getter<boolean> = () => true) =>
	createInfiniteQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.runHistory(undefined),
		initialPageParam: undefined as string | undefined,
		queryFn: async ({ pageParam, signal }): Promise<ScanRunHistoryResponse> => {
			const page = await api.global.v3.GET(LibraryV3Api.scanRunHistory(HISTORY_PAGE, pageParam), {
				signal
			});
			return { items: page.items.map(toScanRun), next_cursor: page.next_cursor ?? null };
		},
		getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined
	}));

export const getLibraryRunFailuresQuery = (getRunId: Getter<string | null>) =>
	createInfiniteQuery(() => {
		const runId = getRunId();
		return {
			enabled: Boolean(runId),
			queryKey: LibraryQueryKeyFactory.runFailures(runId ?? ''),
			initialPageParam: undefined as number | undefined,
			queryFn: async ({ pageParam, signal }): Promise<ScanRunFailuresResponse> => {
				const page = await api.global.v3.GET(
					LibraryV3Api.scanRunFailures(runId ?? '', FAILURES_PAGE, pageParam),
					{ signal }
				);
				return {
					items: page.items.map((item) => ({
						...item,
						phase: item.phase as ScanRunFailuresResponse['items'][number]['phase']
					})),
					next_cursor: page.next_cursor ?? null
				};
			},
			getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined
		};
	});

export const getLibraryRunEstimateQuery = (
	getScopeIds: Getter<string[]>,
	enabled: Getter<boolean>
) =>
	createQuery(() => {
		const scopeIds = getScopeIds();
		return {
			enabled: enabled(),
			queryKey: LibraryQueryKeyFactory.runEstimate(scopeIds),
			queryFn: async ({ signal }): Promise<ScanEstimateResponse> => {
				const estimate = await api.global.v3.GET(LibraryV3Api.scanRunEstimate(scopeIds), {
					signal
				});
				return {
					approximate: estimate.approximate,
					estimated_file_count: estimate.estimated_file_count ?? null,
					estimated_at: estimate.estimated_at ?? null
				};
			}
		};
	});

export const getLibraryOperationQuery = (getJobId: Getter<string | null>) =>
	createQuery(() => {
		const jobId = getJobId();
		return {
			enabled: Boolean(jobId),
			queryKey: LibraryQueryKeyFactory.repair(jobId ?? ''),
			queryFn: ({ signal }) =>
				api.global.get<OperationResponse>(API.library.operation(jobId ?? ''), { signal })
		};
	});
