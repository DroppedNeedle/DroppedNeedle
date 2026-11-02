import { v3 } from '$lib/api/v3/endpoint';
import { ApiError } from '$lib/api/errors';
import {
	applyEffects,
	catalogChanged,
	createAppMutation,
	retagPairsChanged
} from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { toastStore } from '$lib/stores/toast';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';

export type RetagPair = components['schemas']['RetagPairView'];
export type RetagPairPage = components['schemas']['Page_RetagPairView'];

export interface RetagPairJoinInput {
	oldAlbumId: string;
	newAlbumId: string;
	/** Required when the pair's `editions_differ` is true, absent otherwise. */
	keepEditionOf?: string | null;
	/** Each album's `identity_token` as shown, by album id. */
	expectedIdentityTokens: Record<string, string>;
}

// Album rows a scan would not join by itself, newest first. Admin only.
export const getRetagPairsQuery = (enabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.retagPairs(),
		queryFn: async ({ signal }): Promise<RetagPairPage> =>
			api.v3.GET(v3('/api/v3/library/retag-pairs'), { signal })
	}));

// Every pair that names one album, a page at a time. Admin only.
export const getAlbumRetagPairsQuery = (albumId: Getter<string>, enabled: Getter<boolean>) =>
	createInfiniteQuery(() => {
		const album = albumId();
		return {
			enabled: enabled() && Boolean(album),
			queryKey: LibraryQueryKeyFactory.albumRetagPairs(album),
			initialPageParam: undefined as string | undefined,
			queryFn: async ({ pageParam, signal }): Promise<RetagPairPage> =>
				api.v3.GET(
					v3('/api/v3/library/retag-pairs', {
						query: {
							...(album ? { album_id: album } : {}),
							...(pageParam ? { cursor: pageParam } : {})
						}
					}),
					{ signal }
				),
			getNextPageParam: (lastPage: RetagPairPage) => lastPage.next_cursor ?? undefined
		};
	});

const errorMessage = (error: unknown, fallback: string) =>
	error instanceof Error && error.message ? error.message : fallback;

// A 409 means the pair changed since it was shown: read the pairs again.
const refreshIfStale = (error: unknown) =>
	error instanceof ApiError && error.status === 409
		? applyEffects(retagPairsChanged())
		: Promise.resolve();

// A join merges the old album into the new one, so the pair list, both
// albums and every album list change; the library prefix covers them all.
export function createJoinRetagPairMutation() {
	return createAppMutation(() => ({
		mutationFn: (
			input: RetagPairJoinInput
		): Promise<components['schemas']['RetagPairJoinResponse']> =>
			api.v3.POST(
				v3('/api/v3/library/retag-pairs/{old_album_id}/{new_album_id}/join', {
					path: { old_album_id: input.oldAlbumId, new_album_id: input.newAlbumId }
				}),
				{
					keep_edition_of: input.keepEditionOf ?? null,
					expected_identity_tokens: input.expectedIdentityTokens
				}
			),
		onSuccess: () => {
			toastStore.show({ message: 'Albums joined', type: 'success' });
		},
		effects: () => [catalogChanged(authStore.user?.id)],
		onError: (error) => {
			toastStore.show({ message: errorMessage(error, 'Could not join the albums'), type: 'error' });
			return refreshIfStale(error);
		}
	}));
}

export function createDismissRetagPairMutation() {
	return createAppMutation(() => ({
		mutationFn: (input: {
			oldAlbumId: string;
			newAlbumId: string;
		}): Promise<components['schemas']['RetagPairDismissResponse']> =>
			api.v3.POST(
				v3('/api/v3/library/retag-pairs/{old_album_id}/{new_album_id}/dismiss', {
					path: { old_album_id: input.oldAlbumId, new_album_id: input.newAlbumId }
				})
			),
		effects: () => [catalogChanged(authStore.user?.id)],
		onError: (error) => {
			toastStore.show({
				message: errorMessage(error, 'Could not keep the albums apart'),
				type: 'error'
			});
			return refreshIfStale(error);
		}
	}));
}
