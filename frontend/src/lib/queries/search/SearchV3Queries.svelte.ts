import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { ttl } from '$lib/stores/cacheTtl.svelte';
import { SearchV3Api } from './SearchV3Api';
import {
	SearchQueryKeyFactory,
	type SearchV3Bucket,
	type SearchV3Limits,
	type SearchV3UserId
} from './SearchQueryKeyFactory';

export type SearchResponseV3 = components['schemas']['SearchResponse'];
export type SearchBucketResponseV3 = components['schemas']['SearchBucketResponse'];
export type SuggestResponseV3 = components['schemas']['SuggestResponse'];
export type EnrichmentBatchRequestV3 = components['schemas']['EnrichmentBatchRequest'];
export type EnrichmentResponseV3 = components['schemas']['EnrichmentResponse'];

const enabled = (userId: SearchV3UserId, query: string) =>
	Boolean(userId && query.trim().length >= 2);

// A failed remote search used to collapse staleTime to 0, so every tab
// return during a provider outage re-ran the server's MusicBrainz fan-out.
// Failures hold a short floor instead; success keeps the full search window.
const SEARCH_FAILURE_STALE_TIME_MS = 60_000;
const successfulSearchStaleTime = (query: { state: { data?: { status?: string } } }) =>
	query.state.data?.status === 'ok'
		? ttl('search', CACHE_TTL.SEARCH)
		: SEARCH_FAILURE_STALE_TIME_MS;

// A degraded enrichment answers from a partial provider fan-out, so it holds
// briefly; a clean answer keeps the full search window.
export const ENRICH_DEGRADED_STALE_TIME_MS = 60_000;
export const successfulEnrichStaleTime = (query: { state: { data?: EnrichmentResponseV3 } }) =>
	query.state.data && query.state.data.degradations.length > 0
		? ENRICH_DEGRADED_STALE_TIME_MS
		: CACHE_TTL.SEARCH;

const DEFAULT_LIMITS: SearchV3Limits = { artists: 6, albums: 6, tracks: 6 };

// The combined page shows a top hit plus five artist cards and the full
// album shelf; tracks have no shelf there.
export const COMBINED_SEARCH_LIMITS: SearchV3Limits = { artists: 6, albums: 24, tracks: 0 };
// Drill-down pages fetch this many rows per infinite-scroll page.
export const SEARCH_BUCKET_PAGE_SIZE = 24;

// B6, v3 edition: a degraded bucket holds a short floor so an outage never
// refires the provider fan-out on every tab return; a clean answer keeps
// the full search window.
export const successfulUnifiedSearchV3StaleTime = (query: {
	state: { data?: SearchResponseV3 };
}) => {
	const data = query.state.data;
	const healthy =
		data?.artist_status === 'ok' && data?.album_status === 'ok' && data?.track_status === 'ok';
	return healthy ? ttl('search', CACHE_TTL.SEARCH) : SEARCH_FAILURE_STALE_TIME_MS;
};

export const getUnifiedSearchV3QueryOptions = (
	userId: SearchV3UserId,
	query: string,
	limits: SearchV3Limits = DEFAULT_LIMITS,
	buckets: SearchV3Bucket[] | null = null
) =>
	queryOptions({
		enabled: enabled(userId, query),
		staleTime: successfulUnifiedSearchV3StaleTime,
		queryKey: SearchQueryKeyFactory.v3.unified(userId, query, limits, buckets),
		queryFn: ({ signal }) =>
			api.global.v3.GET(SearchV3Api.unified(query, limits, buckets), {
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
		staleTime: successfulSearchStaleTime,
		queryKey: SearchQueryKeyFactory.v3.bucket(userId, bucket, query, limit, offset),
		queryFn: ({ signal }) =>
			api.global.v3.GET(SearchV3Api.bucket(bucket, query, limit, offset), {
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
			staleTime: ttl('search', CACHE_TTL.SEARCH),
			queryKey: SearchQueryKeyFactory.v3.suggest(authStore.user?.id, query, limit),
			queryFn: ({ signal }: { signal?: AbortSignal }) =>
				api.global.v3.GET(SearchV3Api.suggest(query, limit), { signal })
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
		staleTime: successfulEnrichStaleTime,
		queryKey: SearchQueryKeyFactory.v3.enrich(userId, enrichFingerprint(body)),
		queryFn: ({ signal }) => api.global.v3.POST(SearchV3Api.enrichBatch(), body, { signal })
	});

export const getSearchEnrichBatchV3Query = (getBody: Getter<EnrichmentBatchRequestV3>) =>
	createQuery(() => getSearchEnrichBatchV3QueryOptions(authStore.user?.id, getBody()));
