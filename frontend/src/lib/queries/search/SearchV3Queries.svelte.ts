import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { SearchV3Api } from './SearchV3Api';
import {
	SearchQueryKeyFactory,
	type SearchV3Bucket,
	type SearchV3Limits,
	type SearchV3UserId
} from './SearchQueryKeyFactory';
import { PROVIDER } from '$lib/queries/persistedCache';

export type SearchResponseV3 = components['schemas']['SearchResponse'];
export type SearchBucketResponseV3 = components['schemas']['SearchBucketResponse'];
export type SuggestResponseV3 = components['schemas']['SuggestResponse'];
export type EnrichmentBatchRequestV3 = components['schemas']['EnrichmentBatchRequest'];
export type EnrichmentResponseV3 = components['schemas']['EnrichmentResponse'];

const enabled = (userId: SearchV3UserId, query: string) =>
	Boolean(userId && query.trim().length >= 2);

const DEFAULT_LIMITS: SearchV3Limits = { artists: 6, albums: 6, tracks: 6 };

// The combined page shows a top hit, five songs, five artist cards and the
// full album shelf.
export const COMBINED_SEARCH_LIMITS: SearchV3Limits = { artists: 6, albums: 24, tracks: 5 };
// Drill-down pages fetch this many rows per infinite-scroll page.
export const SEARCH_BUCKET_PAGE_SIZE = 24;

export const getUnifiedSearchV3QueryOptions = (
	userId: SearchV3UserId,
	query: string,
	limits: SearchV3Limits = DEFAULT_LIMITS,
	buckets: SearchV3Bucket[] | null = null
) =>
	queryOptions({
		enabled: enabled(userId, query),
		queryKey: SearchQueryKeyFactory.v3.unified(userId, query, limits, buckets),
		queryFn: ({ signal }) =>
			api.v3.GET(SearchV3Api.unified(query, limits, buckets), {
				signal
			})
	});

export const getUnifiedSearchV3Query = (
	getQuery: Getter<string>,
	getLimits: Getter<SearchV3Limits> = () => DEFAULT_LIMITS,
	getBuckets: Getter<SearchV3Bucket[] | null> = () => null
) =>
	createQuery(() =>
		getUnifiedSearchV3QueryOptions(authStore.user?.id, getQuery(), getLimits(), getBuckets())
	);

export const getSearchBucketV3QueryOptions = (
	userId: SearchV3UserId,
	bucket: SearchV3Bucket,
	query: string,
	limit = 24,
	offset = 0
) =>
	queryOptions({
		enabled: enabled(userId, query),
		// The bucket status mirrors the v1 values, so the v1 helper reads it.
		queryKey: SearchQueryKeyFactory.v3.bucket(userId, bucket, query, limit, offset),
		queryFn: ({ signal }) =>
			api.v3.GET(SearchV3Api.bucket(bucket, query, limit, offset), {
				signal
			})
	});

export const getSearchBucketV3Query = (
	getBucket: Getter<SearchV3Bucket>,
	getQuery: Getter<string>,
	getLimit: Getter<number> = () => 24,
	getOffset: Getter<number> = () => 0
) =>
	createQuery(() =>
		getSearchBucketV3QueryOptions(
			authStore.user?.id,
			getBucket(),
			getQuery(),
			getLimit(),
			getOffset()
		)
	);

export const getSearchSuggestionsV3Query = (
	getQuery: Getter<string>,
	getEnabled: Getter<boolean>,
	limit = 5
) =>
	createQuery(() => {
		const query = getQuery().trim();
		return {
			enabled: getEnabled() && enabled(authStore.user?.id, query),
			queryKey: SearchQueryKeyFactory.v3.suggest(authStore.user?.id, query, limit),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.v3.GET(SearchV3Api.suggest(query, limit), { signal })
		};
	});

const enrichFingerprint = (body: EnrichmentBatchRequestV3) => ({
	artists: [...(body.artists ?? [])].map((artist) => artist.musicbrainz_id).sort(),
	albums: [...(body.albums ?? [])].map((album) => album.musicbrainz_id).sort()
});

export const getSearchEnrichBatchV3QueryOptions = (
	userId: SearchV3UserId,
	body: EnrichmentBatchRequestV3
) =>
	queryOptions({
		enabled: Boolean(userId && (body.artists?.length || body.albums?.length)),
		meta: PROVIDER,
		queryKey: SearchQueryKeyFactory.v3.enrich(userId, enrichFingerprint(body)),
		queryFn: ({ signal }) => api.v3.POST(SearchV3Api.enrichBatch(), body, { signal })
	});

export const getSearchEnrichBatchV3Query = (getBody: Getter<EnrichmentBatchRequestV3>) =>
	createQuery(() => getSearchEnrichBatchV3QueryOptions(authStore.user?.id, getBody()));
