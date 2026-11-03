import { WantedQueryKeyFactory } from '$lib/queries/featureKeys';
import type { components } from '$lib/api/v3/openapi';

import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { applyEffects, createAppMutation, wantedChanged } from '$lib/queries/effects';

// Transport types come from the generated v3 contract. The key is
// `musicbrainz_id`.
export type WantedWatchItem = components['schemas']['WantedItem'];
export type WantedRetryingItem = components['schemas']['WantedRetryingItem'];
export type WantedWatchesResponse = components['schemas']['WantedResponse'];
export type WantedActionResponse = components['schemas']['WantedActionResponse'];

export type WantedState = WantedWatchItem['state'];

// Watch rows change only when the background sweep runs; SSE wanted_* events
// invalidate for those. The read-only "still hunting" rows move on the retry
// ladder's minutes-scale though, so a slow poll while the tab is open keeps
// their attempt counters honest.
export const getWantedWatchesQuery = (getEnabled: () => boolean) =>
	createQuery(() => ({
		queryKey: WantedQueryKeyFactory.list(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/requests/wanted'), { signal }),
		enabled: getEnabled() && !!authStore.user?.id,
		refetchInterval: 30_000
	}));

const wanted = () => [wantedChanged(authStore.user?.id)];

export interface WantedActionVars {
	mbid: string;
	albumTitle: string;
}

// mbid is the mutation variable so one instance serves every card in the tab
export const createStopWatchMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ mbid }: WantedActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/wanted/{musicbrainz_id}/stop', { path: { musicbrainz_id: mbid } })
			),
		onSuccess: (_data, { albumTitle }) => {
			toastStore.show({
				message: `Stopped watching ${albumTitle} - it won't be searched again.`,
				type: 'info'
			});
		},
		onError: (_err, { albumTitle }) => {
			toastStore.show({ message: `Couldn't stop watching ${albumTitle}.`, type: 'error' });
			void applyEffects(...wanted());
		},
		effects: wanted
	}));

export const createResumeWatchMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ mbid }: WantedActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/wanted/{musicbrainz_id}/resume', { path: { musicbrainz_id: mbid } })
			),
		onSuccess: (_data, { albumTitle }) => {
			toastStore.show({
				message: `Watching ${albumTitle} - it'll be checked again soon.`,
				type: 'success'
			});
		},
		onError: (_err, { albumTitle }) => {
			toastStore.show({ message: `Couldn't resume watching ${albumTitle}.`, type: 'error' });
			void applyEffects(...wanted());
		},
		effects: wanted
	}));

// silent badge acknowledgment (the mark-seen pattern) - no toast on purpose
export const createMarkWantedSeenMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ mbid }: WantedActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/wanted/{musicbrainz_id}/seen', { path: { musicbrainz_id: mbid } })
			),
		onError: () => void applyEffects(...wanted()),
		effects: wanted
	}));

// The confirm dialog shows a failure, so these raise no error toast.
export const createRemoveWatchMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ mbid }: WantedActionVars) =>
			api.v3.DELETE(
				v3('/api/v3/requests/wanted/{musicbrainz_id}', { path: { musicbrainz_id: mbid } })
			),
		onSuccess: (_data, { albumTitle }) => {
			toastStore.show({ message: `Removed ${albumTitle}.`, type: 'info' });
		},
		effects: wanted
	}));

export const createClearFinishedWatchesMutation = () =>
	createAppMutation(() => ({
		mutationFn: () => api.v3.POST(v3('/api/v3/requests/wanted/clear-finished')),
		onSuccess: ({ removed }) => {
			toastStore.show({
				message: `Removed ${removed} ${removed === 1 ? 'album' : 'albums'}.`,
				type: 'info'
			});
		},
		effects: wanted
	}));
