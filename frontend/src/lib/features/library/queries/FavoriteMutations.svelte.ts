import { v3 } from '$lib/api/v3/endpoint';
import { useQueryClient, type QueryKey } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { catalogChanged, createAppMutation, favoritesChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { withFavorite, type FavoriteChange } from './favoriteRows';

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
			mutationFn: ({ kind, itemId, favorited }: FavoriteChange) =>
				api.v3.PUT(v3('/api/v3/favorites/{kind}/{item_id}', { path: { kind, item_id: itemId } }), {
					favorited
				}),
			onMutate: async (vars: FavoriteChange) => {
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
