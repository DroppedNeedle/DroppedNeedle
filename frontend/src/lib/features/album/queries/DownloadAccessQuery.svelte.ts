import { v3 } from '$lib/api/v3/endpoint';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { LOCAL_KEYS } from '$lib/features/library/queries/LocalV3Keys';
import { STALE_LIVE } from '$lib/queries/staleTime';

// Whether the viewer may download local files.
const getDownloadAccessQueryOptions = (userId: string | undefined) =>
	queryOptions({
		queryKey: LOCAL_KEYS.downloadAccess(userId),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/download/access'), { signal }),
		// The admin can flip the setting at any time: never serve a stale
		// persisted bit (same reasoning as the admin user-quota query).
		staleTime: STALE_LIVE,
		refetchOnMount: 'always' as const
	});

export const getDownloadAccessQuery = (getUserId: Getter<string | undefined>) =>
	createQuery(() => ({
		...getDownloadAccessQueryOptions(getUserId()),
		enabled: Boolean(getUserId())
	}));
