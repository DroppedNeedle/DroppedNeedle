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

// Current runs, history and run detail read the v3 scan routes. Failures,
// estimates and operation jobs have no v3 route yet (see the
// waiting-on-backend list in eslint.config.js).

export const getCurrentLibraryRunsQueryOptions = () =>
	queryOptions({
		queryKey: LibraryQueryKeyFactory.currentRuns(),
		queryFn: async ({ signal }) =>
			toCurrentRuns(await api.global.v3.GET(LibraryV3Api.scanRuns(), { signal })),
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

// v3 returns the recent history in one list, so it reads as a single page.
export const getLibraryRunHistoryQuery = (enabled: Getter<boolean> = () => true) =>
	createInfiniteQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.runHistory(undefined),
		initialPageParam: undefined as string | undefined,
		queryFn: async ({ signal }): Promise<ScanRunHistoryResponse> => {
			const runs = await api.global.v3.GET(LibraryV3Api.scanRuns(), { signal });
			return { items: runs.history.map(toScanRun), next_cursor: null };
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
			queryFn: ({ pageParam, signal }) =>
				api.global.get<ScanRunFailuresResponse>(
					API.library.scanRunFailures(runId ?? '', 50, pageParam),
					{ signal }
				),
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
			queryFn: ({ signal }) =>
				api.global.get<ScanEstimateResponse>(API.library.scanRunEstimate(scopeIds), { signal })
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
