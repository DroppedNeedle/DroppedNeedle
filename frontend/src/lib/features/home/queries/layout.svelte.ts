import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { createQuery } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';
import { everyScope, key, userScope } from '$lib/queries/keys';
import { PERSIST } from '$lib/queries/persistedCache';

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
		enabled: Boolean(authStore.user),
		meta: PERSIST
	}));

/** Save the caller's layout and adopt the server's copy. */
export async function saveHomeLayout(
	update: Omit<HomeLayoutUpdate, 'revision'>,
	revision = 0
): Promise<HomeLayoutResponse> {
	const saved = await api.v3.PUT(ENDPOINT(), {
		...update,
		revision
	});
	await setQueryDataWithPersister(HomeLayoutQueryKeyFactory.layout(authStore.user?.id), saved);
	return saved;
}

/** Re-read every caller's layout, for a posture change. */
export async function refreshHomeLayouts(): Promise<void> {
	await invalidateQueriesWithPersister({ ...everyScope('home-layout'), refetchType: 'active' });
}
