import { RatingQueryKeyFactory } from '$lib/queries/featureKeys';

import { v3 } from '$lib/api/v3/endpoint';
import {
	applyEffects,
	catalogChanged,
	createAppMutation,
	ratingsChanged
} from '$lib/queries/effects';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { QueryKey } from '@tanstack/svelte-query';
import { createSubscriber } from 'svelte/reactivity';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { queryClient, setQueryDataWithPersister } from '$lib/queries/QueryClient';

export type RatingKind = 'album' | 'track' | 'artist';

export type RatingLookup = components['schemas']['RatingLookupResponse'];

/** The local ids a page shows stars for. */
export interface RatingIds {
	albumIds?: string[];
	trackIds?: string[];
	artistIds?: string[];
}

const LOOKUP_FIELD = { album: 'albums', track: 'tracks', artist: 'artists' } as const;
// Where each kind's ids sit in a lookup key, after prefix, user and 'lookup'.
const KEY_POSITION = { album: 3, track: 4, artist: 5 } as const;

/** The caller's rating of one item, 0 when not rated. */
export function ratingOf(lookup: RatingLookup | undefined, kind: RatingKind, id: string): number {
	return lookup?.[LOOKUP_FIELD[kind]][id] ?? 0;
}

function lookupOptions(userId: string | undefined, ids: RatingIds | null) {
	const key = RatingQueryKeyFactory.lookup(
		userId,
		ids?.albumIds ?? [],
		ids?.trackIds ?? [],
		ids?.artistIds ?? []
	);
	const [, , , albumIds, trackIds, artistIds] = key;
	return queryOptions({
		queryKey: key,
		enabled: Boolean(userId && (albumIds.length || trackIds.length || artistIds.length)),
		queryFn: ({ signal }) =>
			api.v3.POST(
				v3('/api/v3/ratings/lookup'),
				{ album_ids: albumIds, track_ids: trackIds, artist_ids: artistIds },
				{ signal }
			)
	});
}

/**
 * The caller's ratings of the ids a page shows, in one read. Pass null
 * until the page knows its ids, so it never reads twice.
 */
export const getRatingsQuery = (getIds: () => RatingIds | null) =>
	createQuery(() => lookupOptions(authStore.user?.id, getIds()));

/** Track id to the caller's rating, 0 when unrated; a track no lookup holds yet is absent. */
export type TrackRatings = ReadonlyMap<string, number>;

// The server reads at most 1000 ids at once; tracks no lookup holds yet are
// read in pieces of this many.
const TRACK_PIECE = 500;

// A change to a cached rating lookup's data, for a store that reads the cache.
const followRatings = createSubscriber((update) =>
	queryClient.getQueryCache().subscribe((event) => {
		if (event.query.queryKey[1] !== RatingQueryKeyFactory.feature) return;
		if (event.type === 'updated' || event.type === 'removed') update();
	})
);

// What the caller's cached lookups say of these tracks, from any page that
// read them; where two disagree, the newer answer wins.
function knownRatings(trackIds: readonly string[]): TrackRatings {
	const wanted = new Set(trackIds);
	const known: [string, number][] = [];
	const lookups = queryClient
		.getQueryCache()
		.findAll({ queryKey: RatingQueryKeyFactory.user(authStore.user?.id) })
		.filter((query) => query.state.data !== undefined)
		.sort((a, b) => a.state.dataUpdatedAt - b.state.dataUpdatedAt);
	for (const query of lookups) {
		const ids = query.queryKey[KEY_POSITION.track];
		if (!Array.isArray(ids)) continue;
		const tracks = (query.state.data as RatingLookup).tracks;
		for (const id of ids) if (wanted.has(id)) known.push([id, tracks[id] ?? 0]);
	}
	// Later entries win, so the newest answer stands.
	return new Map(known);
}

// Lookups for the tracks no cached lookup holds, in pieces the server takes.
function missingPieces(trackIds: readonly string[]) {
	const userId = authStore.user?.id;
	if (!userId) return [];
	const known = knownRatings(trackIds);
	const missing = [...new Set(trackIds)].filter((id) => !known.has(id)).sort();
	const pieces = [];
	for (let start = 0; start < missing.length; start += TRACK_PIECE) {
		pieces.push(lookupOptions(userId, { trackIds: missing.slice(start, start + TRACK_PIECE) }));
	}
	return pieces;
}

/**
 * The cached ratings of these tracks, without reading any. Read inside a
 * derived or an effect, it follows the cache.
 */
export function cachedTrackRatings(trackIds: readonly string[]): TrackRatings {
	followRatings();
	return knownRatings(trackIds);
}

/** Starts reading the ratings of the tracks no lookup holds yet. */
export function warmTrackRatings(trackIds: readonly string[]): void {
	for (const options of missingPieces(trackIds)) void queryClient.prefetchQuery(options);
}

/** These tracks' ratings, reading those no lookup holds yet; throws when a read fails. */
export async function readTrackRatings(trackIds: readonly string[]): Promise<TrackRatings> {
	await Promise.all(missingPieces(trackIds).map((options) => queryClient.fetchQuery(options)));
	return knownRatings(trackIds);
}

interface RateVars {
	kind: RatingKind;
	itemId: string;
	/** 1 to 5, or 0 to clear. */
	rating: number;
}

function lookupHolds(key: QueryKey, { kind, itemId }: RateVars): boolean {
	const ids = key[KEY_POSITION[kind]];
	return Array.isArray(ids) && ids.includes(itemId);
}

function withRating(lookup: RatingLookup, { kind, itemId, rating }: RateVars): RatingLookup {
	const field = LOOKUP_FIELD[kind];
	const next = { ...lookup[field] };
	if (rating > 0) next[itemId] = rating;
	else delete next[itemId];
	return { ...lookup, [field]: next };
}

/**
 * Rate an album, track or artist, or clear it with 0. The stars change at
 * once and go back if the save fails. Ratings save one at a time, so quick
 * clicks end on the last one.
 */
const SET_RATING = ['set-rating'] as const;

export function createSetRatingMutation() {
	return createAppMutation(() => ({
		mutationKey: SET_RATING,
		scope: { id: 'ratings' },
		mutationFn: ({ kind, itemId, rating }: RateVars) =>
			api.v3.PUT(v3('/api/v3/ratings/{kind}/{item_id}', { path: { kind, item_id: itemId } }), {
				rating
			}),
		onMutate: async (vars: RateVars) => {
			const root = RatingQueryKeyFactory.user(authStore.user?.id);
			await queryClient.cancelQueries({ queryKey: root });
			const previous = queryClient
				.getQueriesData<RatingLookup>({ queryKey: root })
				.filter(([key, data]) => data && lookupHolds(key, vars));
			for (const [key, data] of previous) {
				if (data) await setQueryDataWithPersister<RatingLookup>(key, withRating(data, vars));
			}
			return { previous };
		},
		onError: (_error, _vars, context) => {
			for (const [key, data] of context?.previous ?? []) {
				void setQueryDataWithPersister<RatingLookup>(key, data);
			}
			toastStore.show({ message: "Couldn't save the rating.", type: 'error' });
		},
		// The item was merged: move the page to its current ids.
		effects: (answer, vars) =>
			answer.item_id !== vars.itemId ? [catalogChanged(authStore.user?.id)] : [],
		onSettled: () => {
			// A later click is still saving: its own settle reads the result.
			if (queryClient.isMutating({ mutationKey: SET_RATING }) > 1) return;
			void applyEffects(ratingsChanged(authStore.user?.id));
		}
	}));
}
