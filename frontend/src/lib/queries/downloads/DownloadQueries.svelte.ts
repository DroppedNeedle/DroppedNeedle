import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import type { DownloadListResponse } from '$lib/types';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { hasActiveTask } from './downloadStatus';
import { DOWNLOAD_TASKS_ENDPOINTS } from './endpoints';
import { STALE_LIVE } from '$lib/queries/staleTime';

// `downloads.changed` refreshes every download query (see `queries/effects.ts`);
// the interval is only a safety net for a missed event.
const SAFETY_POLL_MS = 60_000;

// The queue summary behind the nav badge. Byte progress arrives as
// `download_progress` events.
export const getDownloadActivitySummaryQueryOptions = () =>
	queryOptions({
		staleTime: STALE_LIVE,
		enabled: !!authStore.user?.id,
		queryKey: DownloadQueryKeyFactory.activity(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.v3.GET(DOWNLOAD_TASKS_ENDPOINTS.activitySummary(), {
				signal
			}),
		refetchInterval: SAFETY_POLL_MS,
		refetchIntervalInBackground: false,
		// reconnect-'always' stays (a reconnect can mean missed SSE events); focus
		// refetches only when the interval-owned data is actually stale (staleTime: 0).
		refetchOnReconnect: 'always' as const
	});

export const getDownloadActivitySummaryQuery = () =>
	createQuery(() => getDownloadActivitySummaryQueryOptions());

// The downloads page owns the bounded detailed projection. It loads on entry and is
// refreshed by `downloads.changed` rather than running a timer of its own.
// No focus-'always': the default stale window keeps same-tab returns free; beyond it
// one stale-true refetch.
export const getDownloadsQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.tasks(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.v3.GET(DOWNLOAD_TASKS_ENDPOINTS.list(undefined, 1, 100), {
				signal
			})
	});

export const getDownloadsQuery = () => createQuery(() => getDownloadsQueryOptions());

// The requests page reads the same list for its rows' live download state.
export const getRequestDownloadsQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		...getDownloadsQueryOptions(),
		enabled: getEnabled() && !!authStore.user?.id,
		refetchInterval: SAFETY_POLL_MS,
		refetchIntervalInBackground: false
	}));

// album-scoped: just this release group's tasks (album + per-track). Cheap (indexed
// on release_group_mbid); the safety poll runs only while a task is in flight. Used by
// the album page for live progress.
export const getAlbumDownloadsQuery = (
	getMbid: Getter<string>,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		staleTime: STALE_LIVE,
		enabled: getEnabled() && !!getMbid(),
		queryKey: DownloadQueryKeyFactory.albumTasks(authStore.user?.id, getMbid()),
		queryFn: ({ signal }) =>
			api.v3.GET(DOWNLOAD_TASKS_ENDPOINTS.list(undefined, 1, 100, getMbid()), {
				signal
			}),
		refetchInterval: (query: { state: { data?: DownloadListResponse | undefined } }) =>
			hasActiveTask(query.state.data?.items ?? []) ? SAFETY_POLL_MS : false
	}));
