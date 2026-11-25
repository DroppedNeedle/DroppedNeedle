import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery, createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { ApiError } from '$lib/api/errors';
import {
	anyDownloadChanged,
	anyRequestChanged,
	createAppMutation,
	discoverPrefsChanged,
	requestsChanged
} from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { libraryStore } from '$lib/stores/library';
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

/** The caller's week, for a page load to read ahead. */
export const getWeekQueryOptions = (userId: string | null | undefined) =>
	queryOptions({
		enabled: Boolean(userId),
		queryKey: DiscoverQueryKeyFactory.v3.week(userId),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/discover/week'), { signal })
	});

/** The caller's week: where it stands and the set to show. */
export const getWeekQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => {
		const options = getWeekQueryOptions(authStore.user?.id);
		return { ...options, enabled: getEnabled() && options.enabled };
	});

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

export type IntakeResponse = components['schemas']['IntakeResponse'];

/** The album a Keep on the counter asks for. */
export interface KeepInput {
	release_group_mbid: string;
	artist_name: string | null;
	album_title: string;
	artist_mbid: string | null;
	made_from: 'weekly_set' | 'dig_further';
}

/** A Keep refused because the caller's request limit is used up, with the limit it names. */
export interface QuotaRefusal {
	limit: number | null;
	window_days: number | null;
}

/** The refusal when `error` is the request limit's (`QUOTA_EXCEEDED`), else null. */
export function quotaRefusal(error: unknown): QuotaRefusal | null {
	if (!(error instanceof ApiError) || error.code !== 'QUOTA_EXCEEDED') return null;
	const details = (error.details ?? {}) as Partial<Record<keyof QuotaRefusal, unknown>>;
	const figure = (value: unknown) => (typeof value === 'number' ? value : null);
	return { limit: figure(details.limit), window_days: figure(details.window_days) };
}

/** The server's own sentence for a refusal it explains (a 4xx), else a plain one. */
function keepFailureMessage(error: unknown): string {
	if (error instanceof ApiError && error.status < 500 && error.message) return error.message;
	return "Couldn't keep that.";
}

/**
 * Keep: asks for the album, as the request routes do from any other page.
 * The counter shows what happened, so only a failure it cannot show (not
 * the request limit) is a toast.
 */
export const createKeepPickMutation = () =>
	createAppMutation(() => ({
		mutationFn: async (input: KeepInput): Promise<IntakeResponse> => {
			const asker = authStore.user?.id;
			let data: IntakeResponse;
			try {
				data = await api.v3.POST(v3('/api/v3/requests/albums'), {
					musicbrainz_id: input.release_group_mbid,
					artist: input.artist_name,
					album: input.album_title,
					artist_mbid: input.artist_mbid,
					made_from: input.made_from
				});
			} catch (error) {
				if (!quotaRefusal(error))
					toastStore.show({ message: keepFailureMessage(error), type: 'error' });
				throw error;
			}
			if (!data.success) {
				toastStore.show({ message: data.message || "Couldn't keep that.", type: 'error' });
				throw new Error(data.message);
			}
			if (asker && asker === authStore.user?.id)
				libraryStore.addRequested(input.release_group_mbid);
			return data;
		},
		effects: () => [requestsChanged(authStore.user?.id), anyDownloadChanged(), anyRequestChanged()]
	}));
