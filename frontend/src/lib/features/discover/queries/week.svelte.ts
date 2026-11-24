import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { createAppMutation, discoverPrefsChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

export type WeekResponse = components['schemas']['WeekResponse'];
export type WeeklySet = components['schemas']['WeeklySet'];
export type SetPick = components['schemas']['SetPick'];
export type DigFurtherPick = components['schemas']['DigFurtherPick'];
export type DiscoverPrefs = components['schemas']['DiscoverPrefs'];
export type SetDay = components['schemas']['SetDay'];

/** "Dig further" albums one page asks for. */
export const MORE_PAGE = 20;

/** The caller's week: where it stands and the set to show. */
export const getWeekQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		queryKey: DiscoverQueryKeyFactory.v3.week(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/discover/week'), { signal })
	}));

/** "Dig further": the caller's pool past their set, a page at a time. */
export const getWeekMoreQuery = (getEnabled: Getter<boolean> = () => true) =>
	createInfiniteQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		queryKey: DiscoverQueryKeyFactory.v3.weekMore(authStore.user?.id),
		initialPageParam: null as string | null,
		queryFn: ({ pageParam, signal }) =>
			api.v3.GET(
				v3('/api/v3/discover/week/more', {
					query: { limit: MORE_PAGE, ...(pageParam ? { cursor: pageParam } : {}) }
				}),
				{ signal }
			),
		getNextPageParam: (lastPage: components['schemas']['Page_DigFurtherPick']) =>
			lastPage.next_cursor ?? undefined
	}));

/** The caller's set day. */
export const getDiscoverPrefsQuery = () =>
	createQuery(() => ({
		enabled: Boolean(authStore.user?.id),
		queryKey: DiscoverQueryKeyFactory.v3.prefs(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/discover/prefs'), { signal })
	}));

/** Keeps the caller's set day; this week's set stays as it is. */
export const createSaveDiscoverPrefsMutation = () =>
	createAppMutation(() => ({
		mutationFn: (prefs: DiscoverPrefs) => api.v3.PUT(v3('/api/v3/discover/prefs'), prefs),
		effects: () => [discoverPrefsChanged(authStore.user?.id)],
		onError: () => toastStore.show({ message: "Couldn't save your set day.", type: 'error' })
	}));
