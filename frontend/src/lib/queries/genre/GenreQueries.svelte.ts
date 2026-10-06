import { createInfiniteQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import type { GenreDetailResponse } from '$lib/types';
import { GenreQueryKeyFactory } from './GenreQueryKeyFactory';
import { toGenreDetail } from '../chartAdapters';
import { HOME_ENDPOINTS } from '../charts/endpoints';

type Getter<T> = () => T;
const PAGE_SIZE = 50;

async function fetchGenrePage(
	genre: string,
	artistOffset: number,
	albumOffset: number,
	signal: AbortSignal
): Promise<GenreDetailResponse> {
	const url = HOME_ENDPOINTS.genre(genre, PAGE_SIZE, artistOffset, albumOffset);
	return toGenreDetail(await api.global.v3.GET(url, { signal }));
}

export const getGenreDetailQuery = (getGenre: Getter<string>) =>
	createInfiniteQuery(() => ({
		staleTime: CACHE_TTL.GENRE_DETAIL,
		queryKey: GenreQueryKeyFactory.artistPages(authStore.user?.id, getGenre()),
		initialPageParam: 0,
		enabled: getGenre().trim().length > 0,
		queryFn: ({ pageParam = 0, signal }) => fetchGenrePage(getGenre(), pageParam, 0, signal),
		getNextPageParam: (lastPage, allPages) =>
			lastPage.popular?.has_more_artists ? allPages.length * PAGE_SIZE : undefined
	}));

export const getGenreAlbumPagesQuery = (getGenre: Getter<string>, getEnabled: Getter<boolean>) =>
	createInfiniteQuery(() => ({
		staleTime: CACHE_TTL.GENRE_DETAIL,
		queryKey: GenreQueryKeyFactory.albumPages(authStore.user?.id, getGenre()),
		initialPageParam: PAGE_SIZE,
		enabled: getGenre().trim().length > 0 && getEnabled(),
		queryFn: ({ pageParam = PAGE_SIZE, signal }) =>
			fetchGenrePage(getGenre(), 0, pageParam, signal),
		getNextPageParam: (lastPage, allPages) =>
			lastPage.popular?.has_more_albums ? PAGE_SIZE + allPages.length * PAGE_SIZE : undefined
	}));
