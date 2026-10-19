import { createMutation, createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { userIdSegment } from '../userKeySegment';
import { SETTINGS_ENDPOINTS } from './endpoints';

export type JellyfinSettings = components['schemas']['JellyfinConnection'];
export type JellyfinVerifyResult = components['schemas']['JellyfinVerifyResponse'];

export const JellyfinSettingsQueryKeyFactory = {
	prefix: ['settings', 'jellyfin'] as const,
	saved: (userId: string | null | undefined) =>
		[...JellyfinSettingsQueryKeyFactory.prefix, userIdSegment(userId)] as const
};

export const getJellyfinSettingsQuery = () =>
	createQuery(() => ({
		queryKey: JellyfinSettingsQueryKeyFactory.saved(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(SETTINGS_ENDPOINTS.jellyfin(), { signal })
	}));

/** The answer is the section as stored, with the API key masked. */
export const createSaveJellyfinSettingsMutation = () =>
	createMutation(() => ({
		mutationFn: (body: JellyfinSettings) => api.global.v3.PUT(SETTINGS_ENDPOINTS.jellyfin(), body),
		onSuccess: (saved: JellyfinSettings) =>
			setQueryDataWithPersister(JellyfinSettingsQueryKeyFactory.saved(authStore.user?.id), saved)
	}));

/** Checks the entered connection without saving it. */
export const createVerifyJellyfinMutation = () =>
	createMutation(() => ({
		mutationFn: (body: JellyfinSettings) =>
			api.global.v3.POST(SETTINGS_ENDPOINTS.jellyfinVerify(), body)
	}));
