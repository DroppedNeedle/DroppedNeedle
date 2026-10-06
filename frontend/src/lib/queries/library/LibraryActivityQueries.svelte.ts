import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import type { LibraryActivityResponse } from './LibraryOperationsTypes';

export const getLibraryActivityQueryOptions = (userId: string | undefined) =>
	queryOptions({
		queryKey: LibraryQueryKeyFactory.activity(userId),
		queryFn: async ({ signal }): Promise<LibraryActivityResponse> =>
			(await api.global.v3.GET(LibraryV3Api.activity(), { signal })) as LibraryActivityResponse,
		staleTime: 2_000
	});

export const getLibraryActivityQuery = (getUserId: Getter<string | undefined>) =>
	createQuery(() => ({
		...getLibraryActivityQueryOptions(getUserId()),
		enabled: Boolean(getUserId())
	}));
