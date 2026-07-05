import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { createMutation } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { PlaylistQueryKeyFactory } from '../playlists/PlaylistQueryKeyFactory';
import { ScrobblePreferencesQueryKeyFactory } from './ScrobblePreferencesQueryKeyFactory';
import { SCROBBLE_PREFERENCES_ENDPOINTS } from './endpoints';
import type {
	PersonalMixRefreshResponse,
	ScrobblePreferences,
	ScrobblePreferencesUpdate
} from './types';

// invalidate so the card + (Phase 5) home/discover re-read the new primary source
export const createUpdateScrobblePreferencesMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ScrobblePreferencesUpdate) =>
			api.global.put<ScrobblePreferences>(SCROBBLE_PREFERENCES_ENDPOINTS.update, vars),
		onSuccess: () =>
			invalidateQueriesWithPersister({
				queryKey: ScrobblePreferencesQueryKeyFactory.get(authStore.user?.id)
			})
	}));

export const createRefreshPersonalMixMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.post<PersonalMixRefreshResponse>(API.me.personalMixRefresh(), {}),
		onSuccess: (data) => {
			invalidateQueriesWithPersister({
				queryKey: PlaylistQueryKeyFactory.list(authStore.user?.id)
			});
			if (data.playlist_id) {
				invalidateQueriesWithPersister({
					queryKey: PlaylistQueryKeyFactory.detail(authStore.user?.id, data.playlist_id)
				});
			}
		}
	}));
