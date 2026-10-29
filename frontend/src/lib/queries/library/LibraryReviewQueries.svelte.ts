import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { LibraryQueryKeyFactory, type LibraryReviewParams } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';

export type LibraryReview = components['schemas']['ReviewView'];
export type LibraryReviewPage = components['schemas']['ReviewsResponse'];

/** The review list, a page at a time: each page carries the count and reason buckets. */
export const getLibraryReviewsQuery = (getParams: Getter<LibraryReviewParams>) =>
	createInfiniteQuery(() => {
		const params = getParams();
		return {
			queryKey: LibraryQueryKeyFactory.reviews(params),
			initialPageParam: undefined as string | undefined,
			queryFn: ({ pageParam, signal }): Promise<LibraryReviewPage> =>
				api.v3.GET(LibraryV3Api.reviews(params, pageParam), { signal }),
			getNextPageParam: (lastPage: LibraryReviewPage) => lastPage.next_cursor ?? undefined
		};
	});

/** How many reviews wait on a person; the library panels show the count. */
export const getPendingReviewCountQuery = () =>
	createQuery(() => {
		const params: LibraryReviewParams = { state: 'pending', limit: 1 };
		return {
			queryKey: LibraryQueryKeyFactory.reviewCount(params),
			queryFn: ({ signal }) => api.v3.GET(LibraryV3Api.reviews(params), { signal }),
			select: (page: LibraryReviewPage) => page.total ?? 0
		};
	});

export const getLibraryReviewQuery = (getReviewId: Getter<string | null>) =>
	createQuery(() => {
		const reviewId = getReviewId();
		return {
			enabled: Boolean(reviewId),
			queryKey: LibraryQueryKeyFactory.review(reviewId ?? ''),
			queryFn: ({ signal }) => api.v3.GET(LibraryV3Api.review(reviewId ?? ''), { signal })
		};
	});
