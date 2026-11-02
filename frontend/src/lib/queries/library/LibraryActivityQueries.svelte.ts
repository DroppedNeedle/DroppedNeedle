import { v3 } from '$lib/api/v3/endpoint';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import type { LibraryActivityResponse } from './LibraryOperationsTypes';
import { STALE_LIVE } from '$lib/queries/staleTime';

const getLibraryActivityQueryOptions = (userId: string | undefined) =>
	queryOptions({
		queryKey: LibraryQueryKeyFactory.activity(userId),
		queryFn: ({ signal }): Promise<LibraryActivityResponse> =>
			api.v3.GET(v3('/api/v3/library/activity'), { signal }),
		staleTime: STALE_LIVE
	});

export const getLibraryActivityQuery = (getUserId: Getter<string | undefined>) =>
	createQuery(() => ({
		...getLibraryActivityQueryOptions(getUserId()),
		enabled: Boolean(getUserId())
	}));
