import { createInfiniteQuery, createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { LIBRARY_MANAGEMENT_ENDPOINTS } from './endpoints';
import {
	toHistory,
	toPlanItemPage,
	toPreviewDetail,
	toRecovery,
	toResultPage,
	toTagEditor
} from './libraryManagementAdapters';

import { LibraryManagementQueryKeyFactory } from './LibraryManagementQueryKeyFactory';
import type {
	LibraryManagementHistoryParams,
	LibraryManagementOperationHistoryResponse,
	LibraryManagementPlanItemPageResponse,
	LibraryManagementPlanItemParams,
	LibraryManagementPreviewDetailResponse,
	LibraryManagementRecoveryDiagnosticsResponse,
	LibraryManagementResultPageResponse,
	LibraryManagementTagEditorContext
} from './types';

type PreviewQuery = { state: { data?: LibraryManagementPreviewDetailResponse } };
type ResultsQuery = {
	state: { data?: { pages: LibraryManagementResultPageResponse[] } };
};

const activePreviewRefetchInterval = (jobId: string | null, query: PreviewQuery) => {
	if (!jobId) return false;
	const preview = query.state.data;
	if (!preview) return 2000;
	if (
		preview.ready_for_confirmation ||
		preview.stale ||
		preview.expired ||
		['failed', 'cancelled', 'discarded', 'stopped', 'succeeded'].includes(preview.state)
	)
		return false;
	return 2000;
};

const activeResultsRefetchInterval = (
	jobId: string | null,
	operationState: string | null,
	query: ResultsQuery
) => {
	if (!jobId) return false;
	const terminalOperationStates = ['failed', 'cancelled', 'discarded', 'stopped', 'succeeded'];
	if (operationState && !terminalOperationStates.includes(operationState)) {
		return 2000;
	}
	const pages = query.state.data?.pages;
	if (!pages) return 2000;
	const hasPendingResults = pages.some((page) =>
		page.items.some(
			(item) => !['succeeded', 'failed', 'skipped', 'not_scheduled'].includes(item.work_state)
		)
	);
	if (!hasPendingResults) return false;
	if (operationState && ['failed', 'cancelled', 'discarded', 'stopped'].includes(operationState)) {
		return false;
	}
	return 2000;
};

export const getLibraryManagementSettingsQueryOptions = (userId: string | null | undefined) =>
	queryOptions({
		queryKey: LibraryManagementQueryKeyFactory.settings(userId),
		queryFn: ({ signal }) => api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.settings(), { signal })
	});

export const getLibraryManagementSettingsQuery = (
	getUserId: Getter<string | null | undefined>,
	enabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		...getLibraryManagementSettingsQueryOptions(getUserId()),
		enabled: enabled()
	}));

export const getLibraryManagementTagEditorQuery = (
	getUserId: Getter<string | null | undefined>,
	getTrackId: Getter<string | null>,
	enabled: Getter<boolean> = () => true
) =>
	createQuery(() => {
		const trackId = getTrackId();
		return {
			enabled: enabled() && Boolean(trackId),
			queryKey: LibraryManagementQueryKeyFactory.tagEditor(getUserId(), trackId ?? ''),
			queryFn: async ({ signal }): Promise<LibraryManagementTagEditorContext> =>
				toTagEditor(
					await api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.tagEditor(trackId ?? ''), {
						signal
					})
				)
		};
	});

export const getLibraryManagementPresetDiffQuery = (
	getUserId: Getter<string | null | undefined>,
	getProfileId: Getter<string | null>
) =>
	createQuery(() => {
		const profileId = getProfileId();
		return {
			enabled: Boolean(profileId),
			queryKey: LibraryManagementQueryKeyFactory.presetDiff(getUserId(), profileId ?? ''),
			queryFn: ({ signal }) =>
				api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.presetDiff(profileId ?? ''), { signal })
		};
	});

export const getLibraryManagementActivationPreviewQuery = (
	getUserId: Getter<string | null | undefined>,
	getJobId: Getter<string | null>
) =>
	createQuery(() => {
		const jobId = getJobId();
		return {
			enabled: Boolean(jobId),
			refetchInterval: (query: PreviewQuery) => activePreviewRefetchInterval(jobId, query),
			queryKey: LibraryManagementQueryKeyFactory.activationPreview(getUserId(), jobId ?? ''),
			queryFn: async ({ signal }): Promise<LibraryManagementPreviewDetailResponse> =>
				toPreviewDetail(
					await api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.activationPreview(jobId ?? ''), {
						signal
					})
				)
		};
	});

export const getLibraryManagementPreviewQuery = (
	getUserId: Getter<string | null | undefined>,
	getJobId: Getter<string | null>
) =>
	createQuery(() => {
		const jobId = getJobId();
		return {
			enabled: Boolean(jobId),
			refetchInterval: (query: PreviewQuery) => activePreviewRefetchInterval(jobId, query),
			queryKey: LibraryManagementQueryKeyFactory.preview(getUserId(), jobId ?? ''),
			queryFn: async ({ signal }) =>
				toPreviewDetail(
					await api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.preview(jobId ?? ''), { signal })
				)
		};
	});

export const getLibraryManagementPlanItemsQuery = (
	getUserId: Getter<string | null | undefined>,
	getJobId: Getter<string | null>,
	getParams: Getter<LibraryManagementPlanItemParams> = () => ({})
) =>
	createInfiniteQuery(() => {
		const jobId = getJobId();
		const params = getParams();
		const limit = params.limit ?? 100;
		return {
			enabled: Boolean(jobId),
			queryKey: LibraryManagementQueryKeyFactory.previewItems(getUserId(), jobId ?? '', params),
			initialPageParam: -1,
			queryFn: async ({ pageParam, signal }) =>
				toPlanItemPage(
					await api.global.v3.GET(
						LIBRARY_MANAGEMENT_ENDPOINTS.previewItems(jobId ?? '', {
							...params,
							afterOrdinal: pageParam,
							limit
						}),
						{ signal }
					)
				),
			getNextPageParam: (lastPage: LibraryManagementPlanItemPageResponse) =>
				lastPage.has_more ? (lastPage.next_after_ordinal ?? undefined) : undefined
		};
	});

export const getLibraryManagementOperationsQuery = (
	getUserId: Getter<string | null | undefined>,
	getParams: Getter<LibraryManagementHistoryParams> = () => ({})
) =>
	createInfiniteQuery(() => {
		const params = getParams();
		return {
			queryKey: LibraryManagementQueryKeyFactory.operations(getUserId(), params),
			initialPageParam: undefined as string | undefined,
			queryFn: async ({ pageParam, signal }) =>
				toHistory(
					await api.global.v3.GET(
						LIBRARY_MANAGEMENT_ENDPOINTS.operations({ ...params, cursor: pageParam }),
						{ signal }
					)
				),
			getNextPageParam: (lastPage: LibraryManagementOperationHistoryResponse) =>
				lastPage.next_cursor ?? undefined
		};
	});

export const getLibraryManagementOperationQuery = (
	getUserId: Getter<string | null | undefined>,
	getJobId: Getter<string | null>
) =>
	createQuery(() => {
		const jobId = getJobId();
		return {
			enabled: Boolean(jobId),
			refetchInterval: (query: PreviewQuery) => activePreviewRefetchInterval(jobId, query),
			queryKey: LibraryManagementQueryKeyFactory.operation(getUserId(), jobId ?? ''),
			queryFn: async ({ signal }) =>
				toPreviewDetail(
					await api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.operation(jobId ?? ''), {
						signal
					})
				)
		};
	});

export const getLibraryManagementOperationResultsQuery = (
	getUserId: Getter<string | null | undefined>,
	getJobId: Getter<string | null>,
	getLimit: Getter<number> = () => 100,
	getOperationState: Getter<string | null> = () => null
) =>
	createInfiniteQuery(() => {
		const jobId = getJobId();
		const limit = getLimit();
		return {
			enabled: Boolean(jobId),
			refetchInterval: (query: ResultsQuery) =>
				activeResultsRefetchInterval(jobId, getOperationState(), query),
			queryKey: LibraryManagementQueryKeyFactory.operationResults(getUserId(), jobId ?? '', limit),
			initialPageParam: -1,
			queryFn: async ({ pageParam, signal }) =>
				toResultPage(
					await api.global.v3.GET(
						LIBRARY_MANAGEMENT_ENDPOINTS.operationResults(jobId ?? '', pageParam, limit),
						{ signal }
					)
				),
			getNextPageParam: (lastPage: LibraryManagementResultPageResponse) =>
				lastPage.has_more ? (lastPage.next_after_ordinal ?? undefined) : undefined
		};
	});

export const getLibraryManagementRecoveryQuery = (
	getUserId: Getter<string | null | undefined>,
	enabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryManagementQueryKeyFactory.recovery(getUserId()),
		queryFn: async ({ signal }): Promise<LibraryManagementRecoveryDiagnosticsResponse> =>
			toRecovery(
				await api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.recoveryDiagnostics(), { signal })
			)
	}));

export const getLibraryManagementAutomaticSkippedQuery = (
	getUserId: Getter<string | null | undefined>,
	enabled: Getter<boolean> = () => true,
	refetchInterval: Getter<number | false> = () => false
) =>
	createQuery(() => ({
		enabled: enabled(),
		refetchInterval: refetchInterval(),
		queryKey: LibraryManagementQueryKeyFactory.automaticSkipped(getUserId()),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.automaticSkipped(), { signal })
	}));

export const TAKEOVERS_POLL_MS = 5_000;

// Albums left unmanaged because their files were changed outside the app,
// and how many of them wait to be handed back. Read again while any wait.
export const getLibraryManagementTakeoversQuery = (
	getUserId: Getter<string | null | undefined>,
	enabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		enabled: enabled(),
		refetchInterval: (query: { state: { data?: { handing_back: number } } }) =>
			(query.state.data?.handing_back ?? 0) > 0 ? TAKEOVERS_POLL_MS : false,
		queryKey: LibraryManagementQueryKeyFactory.takeovers(getUserId()),
		queryFn: ({ signal }) => api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.takeovers(), { signal })
	}));

export const getLibraryManagementActivationHealthQuery = (
	getUserId: Getter<string | null | undefined>,
	enabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryManagementQueryKeyFactory.activationHealth(getUserId()),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.activationHealth(), { signal })
	}));

// How safely each library folder can be written to, as last checked.
// Reading it checks nothing.
export const getLibraryManagementPlacementQuery = (
	getUserId: Getter<string | null | undefined>,
	enabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryManagementQueryKeyFactory.placement(getUserId()),
		queryFn: ({ signal }) => api.global.v3.GET(LIBRARY_MANAGEMENT_ENDPOINTS.placement(), { signal })
	}));
