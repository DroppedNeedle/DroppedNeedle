import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import type { ReviewDetailResponse, ReviewListResponse } from './LibraryOperationsTypes';

export interface LibraryReviewFilters {
	cursor?: string;
	state?: string;
	reasonCode?: string;
	rootId?: string;
	policy?: string;
	search?: string;
	sort?: string;
	candidateAvailable?: boolean;
	hideMatching?: boolean;
}

export const getLibraryReviewsQuery = (getFilters: Getter<LibraryReviewFilters>) =>
	createInfiniteQuery(() => {
		const filters = getFilters();
		return {
			queryKey: LibraryQueryKeyFactory.reviews(filters),
			initialPageParam: filters.cursor,
			queryFn: ({ pageParam, signal }) =>
				api.global.get<ReviewListResponse>(
					API.library.reviews({
						...filters,
						exclude_active_jobs: filters.hideMatching,
						cursor: pageParam,
						limit: 50
					}),
					{ signal }
				),
			getNextPageParam: (lastPage: ReviewListResponse) => lastPage.next_cursor ?? undefined
		};
	});

export const getLibraryReviewQuery = (getReviewId: Getter<string | null>) =>
	createQuery(() => {
		const reviewId = getReviewId();
		return {
			enabled: Boolean(reviewId),
			queryKey: LibraryQueryKeyFactory.review(reviewId ?? ''),
			queryFn: ({ signal }) =>
				api.global.get<ReviewDetailResponse>(API.library.review(reviewId ?? ''), { signal })
		};
	});
