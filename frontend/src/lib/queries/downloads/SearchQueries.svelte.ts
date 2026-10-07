import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { WantedQueryKeyFactory } from '$lib/queries/wanted/WantedQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SEARCH_ENDPOINTS } from './endpoints';

export type SearchAlbumRequest = components['schemas']['SearchAlbumRequest'];
export type SearchAlbumResponse = components['schemas']['SearchAlbumResponse'];
export type SearchJob = components['schemas']['SearchJobResponse'];
export type SearchCandidate = components['schemas']['SearchCandidateView'];
type PickResponse = components['schemas']['PickResponse'];
type DismissSearchResponse = components['schemas']['DismissSearchResponse'];
type ActionResponse = components['schemas']['DownloadActionResponse'];

// Safety net for a missed `search_job_updated` event while the search runs.
const SEARCHING_POLL_MS = 5_000;

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

const invalidateJob = (jobId: string) =>
	invalidateQueriesWithPersister({
		queryKey: DownloadQueryKeyFactory.searchJob(authStore.user?.id, jobId)
	});

export const getSearchJobQueryOptions = (jobId: string) =>
	queryOptions({
		staleTime: 0,
		enabled: jobId !== '',
		queryKey: DownloadQueryKeyFactory.searchJob(authStore.user?.id, jobId),
		queryFn: ({ signal }) =>
			api.global.get<SearchJob>(DOWNLOAD_SEARCH_ENDPOINTS.job(jobId), { signal }),
		refetchInterval: (query: { state: { data?: SearchJob } }) =>
			query.state.data?.status === 'searching' ? SEARCHING_POLL_MS : false
	});

export const getSearchJobQuery = (jobId: () => string) =>
	createQuery(() => getSearchJobQueryOptions(jobId()));

/** Refetch a job when the event stream says it moved. */
export function refreshSearchJob(jobId: string) {
	void invalidateJob(jobId);
}

export function startAlbumSearch() {
	return createMutation(() => ({
		mutationFn: (input: SearchAlbumRequest) =>
			api.global.post<SearchAlbumResponse>(DOWNLOAD_SEARCH_ENDPOINTS.start(), input),
		onSuccess: (data: SearchAlbumResponse) => {
			if (data.status === 'already_in_library') {
				toastStore.show({ message: 'This album is already in your library.', type: 'info' });
			}
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, "Couldn't start the search."), type: 'error' })
	}));
}

export function pickSearchCandidate() {
	return createMutation(() => ({
		mutationFn: (input: { jobId: string; candidate_index: number }) =>
			api.global.post<PickResponse>(DOWNLOAD_SEARCH_ENDPOINTS.pick(input.jobId), {
				candidate_index: input.candidate_index
			}),
		onSuccess: (_data: PickResponse, input: { jobId: string; candidate_index: number }) => {
			toastStore.show({ message: 'Download started from your pick.', type: 'success' });
			void invalidateJob(input.jobId);
			void invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.tasks(authStore.user?.id)
			});
		},
		onError: (err: unknown) =>
			toastStore.show({
				message: errorMessage(err, "Couldn't start that download."),
				type: 'error'
			})
	}));
}

// "None of these - keep watching": closes the search, remembers every
// candidate as turned down, and puts the album on the wanted watchlist.
export function dismissSearch() {
	return createMutation(() => ({
		mutationFn: (jobId: string) =>
			api.global.post<DismissSearchResponse>(DOWNLOAD_SEARCH_ENDPOINTS.dismiss(jobId), {}),
		onSuccess: (_data: DismissSearchResponse, jobId: string) => {
			toastStore.show({
				message: 'On the watchlist - DroppedNeedle will keep checking for it.',
				type: 'success'
			});
			void invalidateJob(jobId);
			void invalidateQueriesWithPersister({
				queryKey: WantedQueryKeyFactory.list(authStore.user?.id)
			});
		},
		onError: (err: unknown) =>
			toastStore.show({
				message: errorMessage(err, "Couldn't move that to the watchlist."),
				type: 'error'
			})
	}));
}

export function cancelSearch() {
	return createMutation(() => ({
		mutationFn: (jobId: string) =>
			api.global.post<ActionResponse>(DOWNLOAD_SEARCH_ENDPOINTS.cancel(jobId), {}),
		onSuccess: (_data: ActionResponse, jobId: string) => invalidateJob(jobId),
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, "Couldn't close the search."), type: 'error' })
	}));
}
