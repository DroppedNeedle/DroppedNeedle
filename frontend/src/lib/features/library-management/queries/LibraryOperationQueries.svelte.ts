import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery, createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from '$lib/features/library/queries/LibraryQueryKeyFactory';
import type {
	ScanEstimateResponse,
	ScanRunFailuresResponse,
	ScanRunHistoryResponse
} from './LibraryOperationsTypes';
import { STALE_LIVE } from '$lib/queries/staleTime';

// Scan runs and operation jobs read the v3 routes.

const HISTORY_PAGE = 50;
const FAILURES_PAGE = 50;

const getCurrentLibraryRunsQueryOptions = () =>
	queryOptions({
		queryKey: LibraryQueryKeyFactory.currentRuns(),
		queryFn: async ({ signal }) =>
			await api.v3.GET(v3('/api/v3/library/scan/runs/current'), { signal }),
		staleTime: STALE_LIVE
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
				await api.v3.GET(v3('/api/v3/library/scan/runs/{id}', { path: { id: runId ?? '' } }), {
					signal
				})
		};
	});

export const getLibraryRunHistoryQuery = (enabled: Getter<boolean> = () => true) =>
	createInfiniteQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.runHistory(undefined),
		initialPageParam: undefined as string | undefined,
		queryFn: async ({ pageParam, signal }): Promise<ScanRunHistoryResponse> => {
			return api.v3.GET(
				v3('/api/v3/library/scan/runs/history', {
					query: { limit: HISTORY_PAGE, ...(pageParam ? { cursor: pageParam } : {}) }
				}),
				{ signal }
			);
		},
		getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined
	}));

export const getLibraryRunFailuresQuery = (getRunId: Getter<string | null>) =>
	createInfiniteQuery(() => {
		const runId = getRunId();
		return {
			enabled: Boolean(runId),
			queryKey: LibraryQueryKeyFactory.runFailures(runId ?? ''),
			initialPageParam: undefined as string | undefined,
			queryFn: async ({ pageParam, signal }): Promise<ScanRunFailuresResponse> => {
				return api.v3.GET(
					v3('/api/v3/library/scan/runs/{id}/failures', {
						path: { id: runId ?? '' },
						query: { limit: FAILURES_PAGE, ...(pageParam ? { cursor: pageParam } : {}) }
					}),
					{
						signal
					}
				);
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
				return api.v3.GET(
					v3('/api/v3/library/scan/runs/estimate', {
						query: scopeIds.length ? { scope_ids: scopeIds } : {}
					}),
					{ signal }
				);
			}
		};
	});
