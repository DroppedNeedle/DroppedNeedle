import { createMutation, createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';
import { LyricsQueryKeyFactory } from '$lib/queries/lyrics/LyricsQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';
import { SettingsQueryKeyFactory } from './SettingsQueryKeyFactory';
import { SETTINGS_ENDPOINTS } from './endpoints';
import { everyScope } from '$lib/queries/keys';

export type LyricsSettings = components['schemas']['LyricsSettings'];

export const getLyricsSettingsQuery = () =>
	createQuery(() => ({
		enabled: authStore.isAdmin,
		queryKey: SettingsQueryKeyFactory.lyrics(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(SETTINGS_ENDPOINTS.lyrics(), { signal })
	}));

/** Saves the switch, then drops cached lyrics so an open panel asks again. */
export const createSaveLyricsSettingsMutation = () =>
	createMutation(() => ({
		mutationFn: (body: LyricsSettings) => api.v3.PUT(SETTINGS_ENDPOINTS.lyrics(), body),
		onSuccess: async (saved: LyricsSettings) => {
			await setQueryDataWithPersister(SettingsQueryKeyFactory.lyrics(authStore.user?.id), saved);
			await invalidateQueriesWithPersister(everyScope(LyricsQueryKeyFactory.feature));
		}
	}));
