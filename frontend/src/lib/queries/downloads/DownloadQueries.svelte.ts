import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import type { DownloadActivitySummary, DownloadListResponse } from '$lib/types';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { hasActiveTask } from './downloadStatus';
import { DOWNLOAD_TASKS_ENDPOINTS } from './endpoints';

const IDLE_ACTIVITY_RECOVERY_MS = 120_000;
const ACTIVE_ACTIVITY_RECOVERY_MS = 60_000;

// The summary is refetched when the event stream says the queue changed
// (`downloads.changed`, see DownloadEvents); its revision then refreshes the detailed
// page after status, ownership, held-review, insert, or delete changes. Byte progress
// arrives as `download_progress` events. The interval is only a slow safety net for a
// missed event.
export const getDownloadActivitySummaryQueryOptions = () =>
	queryOptions({
		staleTime: 0,
		enabled: !!authStore.user?.id,
		queryKey: DownloadQueryKeyFactory.activity(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.get<DownloadActivitySummary>(DOWNLOAD_TASKS_ENDPOINTS.activitySummary(), {
				signal
			}),
		refetchInterval: (query: { state: { data?: DownloadActivitySummary } }) =>
			(query.state.data?.active_count ?? 0) > 0
				? ACTIVE_ACTIVITY_RECOVERY_MS
				: IDLE_ACTIVITY_RECOVERY_MS,
		refetchIntervalInBackground: false,
		// B6: reconnect-'always' stays (a reconnect can mean missed SSE events); focus
		// refetches only when the interval-owned data is actually stale (staleTime: 0).
		refetchOnReconnect: 'always' as const
	});

export const getDownloadActivitySummaryQuery = () =>
	createQuery(() => getDownloadActivitySummaryQueryOptions());

// The downloads page owns the bounded detailed projection. It loads on entry and is
// refreshed by the global summary revision rather than running a second timer.
// B6: no focus-'always' - the SSE reconciler invalidates tasks(userId) exactly, and a
// 30 s stale window keeps same-tab returns free; beyond it one stale-true refetch.
export const getDownloadsQueryOptions = () =>
	queryOptions({
		staleTime: 30_000,
		queryKey: DownloadQueryKeyFactory.tasks(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.get<DownloadListResponse>(DOWNLOAD_TASKS_ENDPOINTS.list(undefined, 1, 100), {
				signal
			})
	});

export const getDownloadsQuery = () => createQuery(() => getDownloadsQueryOptions());

// album-scoped: just this release group's tasks (album + per-track). Cheap (indexed
// on release_group_mbid), and only polls while a task is in flight - it stops itself
// when everything reaches a terminal state. Used by the album page for live progress.
export const getAlbumDownloadsQuery = (
	getMbid: Getter<string>,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		staleTime: 0,
		enabled: getEnabled() && !!getMbid(),
		queryKey: DownloadQueryKeyFactory.albumTasks(authStore.user?.id, getMbid()),
		queryFn: ({ signal }) =>
			api.global.get<DownloadListResponse>(
				DOWNLOAD_TASKS_ENDPOINTS.list(undefined, 1, 100, getMbid()),
				{
					signal
				}
			),
		refetchInterval: (query: { state: { data?: DownloadListResponse | undefined } }) =>
			hasActiveTask(query.state.data?.items ?? []) ? 2500 : false
	}));
