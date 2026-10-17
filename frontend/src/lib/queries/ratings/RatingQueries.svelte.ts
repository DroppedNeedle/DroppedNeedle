import { createMutation, createQuery, type QueryKey } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { invalidateLibraryCatalog } from '$lib/queries/library/LibraryCatalogInvalidation';
import {
	invalidateQueriesWithPersister,
	queryClient,
	setQueryDataWithPersister
} from '../QueryClient';
import { RatingQueryKeyFactory, type RatingKind } from './RatingQueryKeyFactory';
import { RatingV3Api } from './RatingV3Api';

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

/**
 * The caller's ratings of the ids a page shows, in one read. Pass null
 * until the page knows its ids, so it never reads twice.
 */
export const getRatingsQuery = (getIds: () => RatingIds | null) =>
	createQuery(() => {
		const userId = authStore.user?.id;
		const ids = getIds();
		const key = RatingQueryKeyFactory.lookup(
			userId,
			ids?.albumIds ?? [],
			ids?.trackIds ?? [],
			ids?.artistIds ?? []
		);
		const [, , , albumIds, trackIds, artistIds] = key;
		return {
			queryKey: key,
			enabled: Boolean(userId && (albumIds.length || trackIds.length || artistIds.length)),
			queryFn: ({ signal }) =>
				api.global.v3.POST(
					RatingV3Api.lookup(),
					{ album_ids: albumIds, track_ids: trackIds, artist_ids: artistIds },
					{ signal }
				)
		};
	});

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
export function setRating() {
	return createMutation(() => ({
		mutationKey: RatingQueryKeyFactory.prefix,
		scope: { id: 'ratings' },
		mutationFn: ({ kind, itemId, rating }: RateVars) =>
			api.global.v3.PUT(RatingV3Api.rating(kind, itemId), { rating }),
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
		onSuccess: (answer, vars) => {
			// The item was merged: move the page to its current ids.
			if (answer.item_id !== vars.itemId) void invalidateLibraryCatalog();
		},
		onSettled: () => {
			// A later click is still saving: its own settle reads the result.
			if (queryClient.isMutating({ mutationKey: RatingQueryKeyFactory.prefix }) > 1) return;
			void invalidateQueriesWithPersister({
				queryKey: RatingQueryKeyFactory.user(authStore.user?.id)
			});
		}
	}));
}
