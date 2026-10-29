import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { WANTED_ENDPOINTS } from './endpoints';
import { applyEffects, createAppMutation, wantedChanged } from '$lib/queries/effects';

const wanted = () => [wantedChanged(authStore.user?.id)];

export interface WantedActionVars {
	mbid: string;
	albumTitle: string;
}

// mbid is the mutation variable so one instance serves every card in the tab
export const createStopWatchMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ mbid }: WantedActionVars) => api.v3.POST(WANTED_ENDPOINTS.stop(mbid)),
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
		mutationFn: ({ mbid }: WantedActionVars) => api.v3.POST(WANTED_ENDPOINTS.resume(mbid)),
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
		mutationFn: ({ mbid }: WantedActionVars) => api.v3.POST(WANTED_ENDPOINTS.seen(mbid)),
		onError: () => void applyEffects(...wanted()),
		effects: wanted
	}));
