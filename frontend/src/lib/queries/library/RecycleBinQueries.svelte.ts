import { createInfiniteQuery, createMutation, createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';

export type BinRemoval = components['schemas']['BinRemovalView'];
export type RecycleBinPage = components['schemas']['RecycleBinPage'];
export type RecycleBinSummary = components['schemas']['RecycleBinSummary'];
export type RecycleBinStatus = components['schemas']['RecycleBinStatus'];
export type Emptied = components['schemas']['Emptied'];

const PAGE_SIZE = 50;
const POLL_MS = 2000;

// What the recycle bin holds, newest first, a page at a time. Admins only.
export const getRecycleBinQuery = () =>
	createInfiniteQuery(() => ({
		queryKey: LibraryQueryKeyFactory.catalog.recycleBinPages(authStore.user?.id),
		enabled: authStore.isAdmin,
		initialPageParam: null as string | null,
		queryFn: ({ pageParam, signal }) =>
			api.global.v3.GET(LibraryV3Api.recycleBin(pageParam, PAGE_SIZE), { signal }),
		getNextPageParam: (lastPage: RecycleBinPage) => lastPage.next_cursor ?? undefined
	}));

// What Empty would delete now. Read only while the confirm is open, fresh
// each time it opens (the server looks at every copy); its `as_of` bounds
// what Empty may delete.
export const getRecycleBinSummaryQuery = (getOpen: () => boolean) =>
	createQuery(() => ({
		queryKey: LibraryQueryKeyFactory.catalog.recycleBinSummary(authStore.user?.id),
		enabled: authStore.isAdmin && getOpen(),
		staleTime: 0,
		gcTime: 0,
		refetchOnWindowFocus: false,
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.recycleBinSummary(), { signal })
	}));

// Whether the bin is emptying; polled while it is.
export const getRecycleBinStatusQuery = () =>
	createQuery(() => ({
		queryKey: LibraryQueryKeyFactory.catalog.recycleBinStatus(authStore.user?.id),
		enabled: authStore.isAdmin,
		refetchInterval: (query: { state: { data?: RecycleBinStatus } }) =>
			query.state.data?.state === 'emptying' ? POLL_MS : false,
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.recycleBinStatus(), { signal })
	}));

/** Read the bin's page, summary and status again. */
export function invalidateRecycleBin(): Promise<void> {
	return invalidateQueriesWithPersister({
		queryKey: LibraryQueryKeyFactory.catalog.recycleBinPrefix(authStore.user?.id)
	});
}

// Start emptying the bin of what it held at `before` (the summary's
// `as_of`). It runs in the background; the status says when it is done.
export function emptyRecycleBin() {
	return createMutation(() => ({
		mutationFn: (before: number) => api.global.v3.POST(LibraryV3Api.emptyRecycleBin(), { before }),
		onSettled: () =>
			invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.catalog.recycleBinStatus(authStore.user?.id)
			})
	}));
}
