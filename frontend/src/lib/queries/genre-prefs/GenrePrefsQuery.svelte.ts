import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import type { GenrePrefsResponse, GenrePrefsUpdate } from '$lib/types';
import { createQuery } from '@tanstack/svelte-query';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import {
	invalidateQueriesWithPersister,
	setQueryDataWithPersister
} from '$lib/queries/QueryClient';
import { GenrePrefsQueryKeyFactory } from './GenrePrefsQueryKeyFactory';

export const getGenrePrefsQuery = () =>
	createQuery(() => ({
		staleTime: 60_000,
		queryKey: GenrePrefsQueryKeyFactory.prefs(authStore.user?.id),
		queryFn: ({ signal }) => api.global.get<GenrePrefsResponse>(API.me.genrePrefs(), { signal })
	}));

/** Save the user's genre balance levels; refreshes the prefs cache and
 * invalidates the Discover data (incl. the taste graph) since the backend
 * rebuilds recommendations with the new levels. */
export async function saveGenrePrefs(update: GenrePrefsUpdate): Promise<void> {
	const saved = await api.global.put<GenrePrefsResponse>(API.me.genrePrefs(), update);
	const userId = authStore.user?.id;
	await setQueryDataWithPersister<GenrePrefsResponse>(
		GenrePrefsQueryKeyFactory.prefs(userId),
		() => saved
	);
	await invalidateQueriesWithPersister({ queryKey: DiscoverQueryKeyFactory.prefix });
}
