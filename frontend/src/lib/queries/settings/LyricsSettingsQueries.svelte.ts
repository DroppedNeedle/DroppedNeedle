import { createMutation, createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';
import { LyricsQueryKeyFactory } from '$lib/queries/lyrics/LyricsQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';
import { userIdSegment } from '../userKeySegment';
import { SETTINGS_ENDPOINTS } from './endpoints';

export type LyricsSettings = components['schemas']['LyricsSettings'];

export const LyricsSettingsQueryKeyFactory = {
	prefix: ['settings', 'lyrics'] as const,
	saved: (userId: string | null | undefined) =>
		[...LyricsSettingsQueryKeyFactory.prefix, userIdSegment(userId)] as const
};

export const getLyricsSettingsQuery = () =>
	createQuery(() => ({
		enabled: authStore.isAdmin,
		queryKey: LyricsSettingsQueryKeyFactory.saved(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(SETTINGS_ENDPOINTS.lyrics(), { signal })
	}));

/** Saves the switch, then drops cached lyrics so an open panel asks again. */
export const createSaveLyricsSettingsMutation = () =>
	createMutation(() => ({
		mutationFn: (body: LyricsSettings) => api.global.v3.PUT(SETTINGS_ENDPOINTS.lyrics(), body),
		onSuccess: async (saved: LyricsSettings) => {
			await setQueryDataWithPersister(
				LyricsSettingsQueryKeyFactory.saved(authStore.user?.id),
				saved
			);
			await invalidateQueriesWithPersister({ queryKey: LyricsQueryKeyFactory.prefix });
		}
	}));
