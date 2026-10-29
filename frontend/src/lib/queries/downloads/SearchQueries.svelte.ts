import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SEARCH_ENDPOINTS } from './endpoints';
import { STALE_LIVE } from '$lib/queries/staleTime';
import {
	createAppMutation,
	downloadsChanged,
	searchJobChanged,
	wantedChanged
} from '$lib/queries/effects';

export type SearchAlbumRequest = components['schemas']['SearchAlbumRequest'];
export type SearchAlbumResponse = components['schemas']['SearchAlbumResponse'];
export type SearchJob = components['schemas']['SearchJobResponse'];
export type SearchCandidate = components['schemas']['SearchCandidateView'];
export type CandidateRefusal = components['schemas']['CandidateRefusal'];
type PickRequest = components['schemas']['PickRequest'];
type PickInput = PickRequest & { jobId: string };
type PickResponse = components['schemas']['PickResponse'];
type DismissSearchResponse = components['schemas']['DismissSearchResponse'];
type ActionResponse = components['schemas']['DownloadActionResponse'];

// Safety net for a missed `search_job_updated` event while the search runs.
const SEARCHING_POLL_MS = 60_000;

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

export const getSearchJobQueryOptions = (jobId: string) =>
	queryOptions({
		staleTime: STALE_LIVE,
		enabled: jobId !== '',
		queryKey: DownloadQueryKeyFactory.searchJob(authStore.user?.id, jobId),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_SEARCH_ENDPOINTS.job(jobId), { signal }),
		refetchInterval: (query: { state: { data?: SearchJob } }) =>
			query.state.data?.status === 'searching' ? SEARCHING_POLL_MS : false
	});

export const getSearchJobQuery = (jobId: () => string) =>
	createQuery(() => getSearchJobQueryOptions(jobId()));

export function startAlbumSearch() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (input: SearchAlbumRequest) =>
			api.v3.POST(DOWNLOAD_SEARCH_ENDPOINTS.start(), input),
		onSuccess: (data: SearchAlbumResponse) => {
			if (data.status === 'already_in_library') {
				toastStore.show({ message: 'This album is already in your library.', type: 'info' });
			}
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, "Couldn't start the search."), type: 'error' })
	}));
}

/**
 * The reason a pick was refused for being outside the download settings
 * (a 409 the server sends when the page did not ask first), or null.
 */
export function policyBreak(err: unknown): CandidateRefusal | null {
	if (!(err instanceof ApiError) || err.status !== 409) return null;
	const details = err.details as { refusal?: CandidateRefusal } | null;
	return details?.refusal ?? null;
}

export function pickSearchCandidate() {
	return createAppMutation(() => ({
		mutationFn: ({ jobId, ...body }: PickInput) =>
			api.v3.POST(DOWNLOAD_SEARCH_ENDPOINTS.pick(jobId), body),
		onSuccess: () => {
			toastStore.show({ message: 'Download started from your pick.', type: 'success' });
		},
		effects: (_data: PickResponse, input: PickInput) => [
			searchJobChanged(authStore.user?.id, input.jobId),
			downloadsChanged(authStore.user?.id)
		],
		onError: (err: unknown) => {
			// The page asks first; it opens that question when the server asks.
			if (policyBreak(err)) return;
			toastStore.show({
				message: errorMessage(err, "Couldn't start that download."),
				type: 'error'
			});
		}
	}));
}

// "None of these - keep watching": closes the search, remembers every
// candidate as turned down, and puts the album on the wanted watchlist.
export function dismissSearch() {
	return createAppMutation(() => ({
		mutationFn: (jobId: string) => api.v3.POST(DOWNLOAD_SEARCH_ENDPOINTS.dismiss(jobId)),
		onSuccess: () => {
			toastStore.show({
				message: 'On the watchlist - DroppedNeedle will keep checking for it.',
				type: 'success'
			});
		},
		effects: (_data: DismissSearchResponse, jobId: string) => [
			searchJobChanged(authStore.user?.id, jobId),
			wantedChanged(authStore.user?.id)
		],
		onError: (err: unknown) =>
			toastStore.show({
				message: errorMessage(err, "Couldn't move that to the watchlist."),
				type: 'error'
			})
	}));
}

export function cancelSearch() {
	return createAppMutation(() => ({
		mutationFn: (jobId: string) => api.v3.POST(DOWNLOAD_SEARCH_ENDPOINTS.cancel(jobId)),
		effects: (_data: ActionResponse, jobId: string) => [
			searchJobChanged(authStore.user?.id, jobId)
		],
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, "Couldn't close the search."), type: 'error' })
	}));
}
