import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { setQueryDataWithPersister } from '../QueryClient';
import { ListenBrainzQueryKeyFactory } from './ListenBrainzQueryKeyFactory';
import { LISTENBRAINZ_ENDPOINTS } from './endpoints';
import type { ListenBrainzConnectionDto, ScrobbleSettingsDto } from './types';

// Save mutations refresh the cache and stay quiet: the settings tab shows
// inline receipts, which read better than toasts on a form.
export const createSaveListenBrainzMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ListenBrainzConnectionDto) =>
			api.global.v3.PUT(LISTENBRAINZ_ENDPOINTS.connection, vars),
		onMutate: () => ({ admin: authStore.isAdmin }),
		onSuccess: async (saved, _vars, context) => {
			if (context.admin && authStore.isAdmin) {
				await setQueryDataWithPersister(ListenBrainzQueryKeyFactory.connection(), saved);
			}
		}
	}));

export const createSaveScrobbleTargetsMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ScrobbleSettingsDto) =>
			api.global.v3.PUT(LISTENBRAINZ_ENDPOINTS.scrobble, vars),
		onMutate: () => ({ admin: authStore.isAdmin }),
		onSuccess: async (saved, _vars, context) => {
			if (context.admin && authStore.isAdmin) {
				await setQueryDataWithPersister(ListenBrainzQueryKeyFactory.scrobble(), saved);
			}
		}
	}));

// Verify only probes the submitted values; the tab renders the verdict.
export const createVerifyListenBrainzMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ListenBrainzConnectionDto) =>
			api.global.v3.POST(LISTENBRAINZ_ENDPOINTS.verify, vars)
	}));
