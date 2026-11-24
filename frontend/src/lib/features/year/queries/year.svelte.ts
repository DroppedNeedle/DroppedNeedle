import { createQuery } from '@tanstack/svelte-query';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { YearReviewQueryKeyFactory } from '$lib/queries/featureKeys';

export type YearReviewYears = components['schemas']['YearReviewYears'];
export type YearReview = components['schemas']['YearReview'];

// The year is counted in the calendar of this device's time zone.
const deviceZone = () => Intl.DateTimeFormat().resolvedOptions().timeZone;

export const getYearReviewYearsQuery = () => {
	const zone = deviceZone();
	return createQuery(() => ({
		queryKey: YearReviewQueryKeyFactory.years(authStore.user?.id, zone),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/me/year-in-review', { query: { zone } }), { signal }),
		enabled: Boolean(authStore.user?.id)
	}));
};

export const getYearReviewQuery = (year: () => number) => {
	const zone = deviceZone();
	return createQuery(() => ({
		queryKey: YearReviewQueryKeyFactory.year(authStore.user?.id, year(), zone),
		queryFn: ({ signal }) =>
			api.v3.GET(
				v3('/api/v3/me/year-in-review/{year}', { path: { year: year() }, query: { zone } }),
				{ signal }
			),
		enabled: Boolean(authStore.user?.id)
	}));
};
