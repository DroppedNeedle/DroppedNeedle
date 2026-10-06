import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import type { DownloadAccessResponse } from '$lib/types';
import { LOCAL_KEYS } from './LocalV3Keys';

// Whether the viewer may download local files. v3 has no route for this yet,
// so it still asks the old path (see the waiting-on-backend list in
// eslint.config.js).
export const getDownloadAccessQueryOptions = (userId: string | undefined) =>
	queryOptions({
		queryKey: LOCAL_KEYS.downloadAccess(userId),
		queryFn: ({ signal }) =>
			api.global.get<DownloadAccessResponse>(API.download.access(), { signal }),
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
