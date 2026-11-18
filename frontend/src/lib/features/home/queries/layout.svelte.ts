import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { createQuery } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';
import { key, userScope } from '$lib/queries/keys';

export type HomeLayoutResponse = components['schemas']['HomeLayoutResponse'];
export type HomeLayoutUpdate = components['schemas']['HomeLayoutUpdate'];
export type HomeLayoutWidget = components['schemas']['HomeLayoutWidget'];

export const HomeLayoutQueryKeyFactory = {
	layout: (userId: string | null | undefined) => key(userScope(userId), 'home-layout')
};

const ENDPOINT = () => v3('/api/v3/me/home-layout');

/** The layout the caller should see: their own, the admin's, or the default. */
export const getHomeLayoutQuery = () =>
	createQuery(() => ({
		queryKey: HomeLayoutQueryKeyFactory.layout(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(ENDPOINT(), { signal }),
		enabled: Boolean(authStore.user)
	}));

/** Save the caller's layout and adopt the server's copy. */
export async function saveHomeLayout(update: HomeLayoutUpdate): Promise<HomeLayoutResponse> {
	const saved = await api.v3.PUT(ENDPOINT(), update);
	await setQueryDataWithPersister(HomeLayoutQueryKeyFactory.layout(authStore.user?.id), saved);
	return saved;
}

/** Re-read every caller's layout, for a posture change. */
export async function refreshHomeLayouts(): Promise<void> {
	await invalidateQueriesWithPersister({ queryKey: ['home-layout'], refetchType: 'active' });
}
