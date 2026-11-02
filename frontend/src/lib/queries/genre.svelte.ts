import { GenreQueryKeyFactory } from '$lib/queries/featureKeys';

import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import type { GenreDetailResponse } from '$lib/queries/discover/types';
import { PERSIST } from '$lib/queries/persistedCache';

type Getter<T> = () => T;
const PAGE_SIZE = 50;

async function fetchGenrePage(
	genre: string,
	artistOffset: number,
	albumOffset: number,
	signal: AbortSignal
): Promise<GenreDetailResponse> {
	const url = v3('/api/v3/home/genre/{genre_name}', {
		path: { genre_name: genre },
		query: { limit: PAGE_SIZE, artist_offset: artistOffset, album_offset: albumOffset }
	});
	return api.v3.GET(url, { signal });
}

export const getGenreDetailQuery = (getGenre: Getter<string>) =>
	createInfiniteQuery(() => ({
		meta: PERSIST,
		queryKey: GenreQueryKeyFactory.artistPages(authStore.user?.id, getGenre()),
		initialPageParam: 0,
		enabled: getGenre().trim().length > 0,
		queryFn: ({ pageParam = 0, signal }) => fetchGenrePage(getGenre(), pageParam, 0, signal),
		getNextPageParam: (lastPage, allPages) =>
			lastPage.popular?.has_more_artists ? allPages.length * PAGE_SIZE : undefined
	}));

export const getGenreAlbumPagesQuery = (getGenre: Getter<string>, getEnabled: Getter<boolean>) =>
	createInfiniteQuery(() => ({
		meta: PERSIST,
		queryKey: GenreQueryKeyFactory.albumPages(authStore.user?.id, getGenre()),
		initialPageParam: PAGE_SIZE,
		enabled: getGenre().trim().length > 0 && getEnabled(),
		queryFn: ({ pageParam = PAGE_SIZE, signal }) =>
			fetchGenrePage(getGenre(), 0, pageParam, signal),
		getNextPageParam: (lastPage, allPages) =>
			lastPage.popular?.has_more_albums ? PAGE_SIZE + allPages.length * PAGE_SIZE : undefined
	}));
