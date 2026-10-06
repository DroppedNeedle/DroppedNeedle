import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import type { DownloadAccessResponse } from '$lib/types';
import { LocalDownloadsApi } from './LocalDownloadsApi';
import { LOCAL_KEYS } from './LocalV3Keys';

// Whether the viewer may download local files.
export const getDownloadAccessQueryOptions = (userId: string | undefined) =>
	queryOptions({
		queryKey: LOCAL_KEYS.downloadAccess(userId),
		queryFn: async ({ signal }): Promise<DownloadAccessResponse> =>
			api.global.v3.GET(LocalDownloadsApi.access(), { signal }),
		// The admin can flip the setting at any time: never serve a stale
		// persisted bit (same reasoning as the admin user-quota query).
		staleTime: 0,
		refetchOnMount: 'always' as const
	});

export const getDownloadAccessQuery = (getUserId: Getter<string | undefined>) =>
	createQuery(() => ({
		...getDownloadAccessQueryOptions(getUserId()),
		enabled: Boolean(getUserId())
	}));
