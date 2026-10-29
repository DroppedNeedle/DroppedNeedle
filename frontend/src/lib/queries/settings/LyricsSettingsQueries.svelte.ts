import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { SettingsQueryKeyFactory } from './SettingsQueryKeyFactory';
import { SETTINGS_ENDPOINTS } from './endpoints';
import { createAppMutation, lyricsChanged } from '$lib/queries/effects';

export type LyricsSettings = components['schemas']['LyricsSettings'];

export const getLyricsSettingsQuery = () =>
	createQuery(() => ({
		enabled: authStore.isAdmin,
		queryKey: SettingsQueryKeyFactory.lyrics(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(SETTINGS_ENDPOINTS.lyrics(), { signal })
	}));

/** Saves the switch, then drops cached lyrics so an open panel asks again. */
export const createSaveLyricsSettingsMutation = () =>
	createAppMutation(() => ({
		mutationFn: (body: LyricsSettings) => api.v3.PUT(SETTINGS_ENDPOINTS.lyrics(), body),
		onSuccess: (saved: LyricsSettings) =>
			setQueryDataWithPersister(SettingsQueryKeyFactory.lyrics(authStore.user?.id), saved),
		effects: () => [lyricsChanged()]
	}));
