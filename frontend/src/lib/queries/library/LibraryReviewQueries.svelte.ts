import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { LibraryQueryKeyFactory, type LibraryReviewParams } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';

export type LibraryReview = components['schemas']['ReviewView'];
export type LibraryReviewPage = components['schemas']['ReviewsResponse'];

export const getLibraryReviewsQuery = (getParams: Getter<LibraryReviewParams>) =>
	createQuery(() => {
		const params = getParams();
		return {
			queryKey: LibraryQueryKeyFactory.reviews(params),
			queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.reviews(params), { signal })
		};
	});

/** How many reviews wait on a person; the library panels show the count. */
export const getPendingReviewCountQuery = () =>
	createQuery(() => {
		const params: LibraryReviewParams = { state: 'pending', limit: 1, offset: 0 };
		return {
			queryKey: LibraryQueryKeyFactory.reviews(params),
			queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.reviews(params), { signal }),
			select: (page: LibraryReviewPage) => page.total
		};
	});

export const getLibraryReviewQuery = (getReviewId: Getter<string | null>) =>
	createQuery(() => {
		const reviewId = getReviewId();
		return {
			enabled: Boolean(reviewId),
			queryKey: LibraryQueryKeyFactory.review(reviewId ?? ''),
			queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.review(reviewId ?? ''), { signal })
		};
	});
