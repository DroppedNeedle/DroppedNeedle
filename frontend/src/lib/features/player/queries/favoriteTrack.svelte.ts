import { createInfiniteQuery, useQueryClient } from '@tanstack/svelte-query';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from '$lib/features/library/queries/LibraryQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';

// The caller's favourite tracks, the most a page holds at a time. It sits
// under the catalog's key, so a heart given anywhere marks it for a read.
const PARAMS = { limit: 500, favorites: true } as const;
// A list longer than this many pages is not read further.
const MAX_PAGES = 20;

/**
 * Whether the caller hearted this library track, read from their own
 * favourites list (no read answers for one track). False until it is read.
 */
export function getTrackFavoriteQuery(getTrackId: () => string | null) {
	const client = useQueryClient();
	const key = () => LibraryQueryKeyFactory.catalog.tracks(authStore.user?.id, PARAMS);
	const query = createInfiniteQuery(() => ({
		queryKey: key(),
		enabled: Boolean(authStore.user?.id && getTrackId()),
		initialPageParam: null as string | null,
		queryFn: ({ pageParam, signal }) =>
			api.v3.GET(
				v3('/api/v3/library/tracks', {
					query: { ...PARAMS, ...(pageParam ? { after: pageParam } : {}) }
				}),
				{ signal }
			),
		getNextPageParam: (page, pages) =>
			pages.length < MAX_PAGES ? (page.next ?? undefined) : undefined
	}));

	$effect(() => {
		if (query.hasNextPage && !query.isFetchingNextPage) void query.fetchNextPage();
	});

	// A heart patches the lists that hold its row and only marks the rest;
	// a new favourite has no row here yet, so a marked list is read again.
	$effect(() => {
		if (!query.isStale || query.isFetching || !getTrackId()) return;
		if (client.getQueryState(key())?.isInvalidated) void query.refetch();
	});

	return {
		get favorite(): boolean {
			const id = getTrackId();
			return Boolean(
				id &&
				query.data?.pages.some((page) =>
					page.items.some((track) => track.id === id && track.favorite)
				)
			);
		},
		/** When the list was last read, so a heart being saved knows when it shows. */
		get readAt(): number {
			return query.dataUpdatedAt;
		}
	};
}
