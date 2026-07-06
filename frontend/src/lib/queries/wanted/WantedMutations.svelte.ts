import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { WantedQueryKeyFactory } from './WantedQueryKeyFactory';
import { WANTED_ENDPOINTS } from './endpoints';
import type { WantedActionResponse } from './types';

function invalidateWanted(): Promise<void> {
	return invalidateQueriesWithPersister({
		queryKey: WantedQueryKeyFactory.list(authStore.user?.id)
	});
}

export interface WantedActionVars {
	mbid: string;
	albumTitle: string;
}

// mbid is the mutation variable so one instance serves every card in the tab
export const createStopWatchMutation = () =>
	createMutation(() => ({
		mutationFn: ({ mbid }: WantedActionVars) =>
			api.global.post<WantedActionResponse>(WANTED_ENDPOINTS.stop(mbid)),
		onSuccess: (_data, { albumTitle }) => {
			toastStore.show({
				message: `Stopped watching ${albumTitle} - it won't be searched again.`,
				type: 'info'
			});
		},
		onError: (_err, { albumTitle }) => {
			toastStore.show({ message: `Couldn't stop watching ${albumTitle}.`, type: 'error' });
		},
		onSettled: () => {
			void invalidateWanted();
		}
	}));

export const createResumeWatchMutation = () =>
	createMutation(() => ({
		mutationFn: ({ mbid }: WantedActionVars) =>
			api.global.post<WantedActionResponse>(WANTED_ENDPOINTS.resume(mbid)),
		onSuccess: (_data, { albumTitle }) => {
			toastStore.show({
				message: `Watching ${albumTitle} - it'll be checked again soon.`,
				type: 'success'
			});
		},
		onError: (_err, { albumTitle }) => {
			toastStore.show({ message: `Couldn't resume watching ${albumTitle}.`, type: 'error' });
		},
		onSettled: () => {
			void invalidateWanted();
		}
	}));

// silent badge acknowledgment (the mark-seen pattern) - no toast on purpose
export const createMarkWantedSeenMutation = () =>
	createMutation(() => ({
		mutationFn: ({ mbid }: WantedActionVars) =>
			api.global.post<WantedActionResponse>(WANTED_ENDPOINTS.seen(mbid)),
		onSettled: () => {
			void invalidateWanted();
		}
	}));
