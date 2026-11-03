import { v3 } from '$lib/api/v3/endpoint';
import { useQueryClient, type QueryKey } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { catalogChanged, createAppMutation, favoritesChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';

export type FavoriteKind = 'album' | 'track' | 'artist';

interface FavoriteVars {
	kind: FavoriteKind;
	itemId: string;
	favorited: boolean;
}

type Row = Record<string, unknown>;

// A field only each kind's view has, so an album never takes a
// track's heart when two ids match.
const KIND_FIELD: Record<FavoriteKind, string> = {
	album: 'is_compilation',
	artist: 'led_track_count',
	track: 'track_number'
};

/** `data` with the item's flag set, or `data` itself when nothing in it changes. */
function withFavorite(data: unknown, vars: FavoriteVars): unknown {
	if (Array.isArray(data)) {
		const next = data.map((item) => withFavorite(item, vars));
		return next.some((item, index) => item !== data[index]) ? next : data;
	}
	if (!data || typeof data !== 'object') return data;
	const row = data as Row;
	if (row.id === vars.itemId && KIND_FIELD[vars.kind] in row && 'favorite' in row) {
		return row.favorite === vars.favorited ? data : { ...row, favorite: vars.favorited };
	}
	let changed = false;
	const next: Row = {};
	for (const [field, value] of Object.entries(row)) {
		next[field] = withFavorite(value, vars);
		if (next[field] !== value) changed = true;
	}
	return changed ? next : data;
}

// Every cached read that carries the caller's favourite flags.
function flaggedRoots(): QueryKey[] {
	const userId = authStore.user?.id;
	return [LibraryQueryKeyFactory.catalog.root(userId), LibraryQueryKeyFactory.albumPrefix(userId)];
}

/**
 * Heart or unheart an album, track or artist: the caller's favourites,
 * the same ones player apps star. The heart changes at once and goes back
 * if the save fails. Saves run one at a time, so quick clicks end on the
 * last one.
 */
export function createSetFavoriteMutation() {
	const client = useQueryClient();
	return createAppMutation(
		() => ({
			scope: { id: 'favorites' },
			mutationFn: ({ kind, itemId, favorited }: FavoriteVars) =>
				api.v3.PUT(v3('/api/v3/favorites/{kind}/{item_id}', { path: { kind, item_id: itemId } }), {
					favorited
				}),
			onMutate: async (vars: FavoriteVars) => {
				const previous: [QueryKey, unknown][] = [];
				for (const root of flaggedRoots()) {
					await client.cancelQueries({ queryKey: root });
					for (const [key, data] of client.getQueriesData({ queryKey: root })) {
						const next = withFavorite(data, vars);
						if (next === data) continue;
						previous.push([key, data]);
						await setQueryDataWithPersister(key, next, undefined, client);
					}
				}
				return { previous };
			},
			onError: (_error, _vars, context) => {
				for (const [key, data] of context?.previous ?? []) {
					void setQueryDataWithPersister(key, data, undefined, client);
				}
				toastStore.show({ message: "Couldn't save the favourite.", type: 'error' });
			},
			// The item was merged: move the page to its current ids.
			effects: (answer, vars) => [
				favoritesChanged(authStore.user?.id),
				...(answer.item_id !== vars.itemId ? [catalogChanged(authStore.user?.id)] : [])
			]
		}),
		client
	);
}
