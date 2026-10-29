import { ApiError } from '$lib/api/errors';
import {
	applyEffects,
	catalogChanged,
	createAppMutation,
	retagPairsChanged
} from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { toastStore } from '$lib/stores/toast';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';

export type RetagPair = components['schemas']['RetagPairView'];
export type RetagPairList = components['schemas']['RetagPairListResponse'];

export interface RetagPairJoinInput {
	oldAlbumId: string;
	newAlbumId: string;
	/** Required when the pair's `editions_differ` is true, absent otherwise. */
	keepEditionOf?: string | null;
	/** Each album's `identity_revision` as shown, by album id. */
	expectedIdentityRevisions: Record<string, number>;
}

// Album rows a scan would not join by itself, newest first. Admin only.
export const getRetagPairsQuery = (enabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.retagPairs(),
		queryFn: async ({ signal }): Promise<RetagPairList> =>
			api.v3.GET(LibraryV3Api.retagPairs(), { signal })
	}));

const errorMessage = (error: unknown, fallback: string) =>
	error instanceof Error && error.message ? error.message : fallback;

// A 409 means the pair changed since it was shown: read the pairs again.
const refreshIfStale = (error: unknown) =>
	error instanceof ApiError && error.status === 409
		? applyEffects(retagPairsChanged())
		: Promise.resolve();

// A join merges the old album into the new one, so the pair list, both
// albums and every album list change; the library prefix covers them all.
export function joinRetagPair() {
	return createAppMutation(() => ({
		mutationFn: (
			input: RetagPairJoinInput
		): Promise<components['schemas']['RetagPairJoinResponse']> =>
			api.v3.POST(LibraryV3Api.joinRetagPair(input.oldAlbumId, input.newAlbumId), {
				keep_edition_of: input.keepEditionOf ?? null,
				expected_identity_revisions: input.expectedIdentityRevisions
			}),
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

export function dismissRetagPair() {
	return createAppMutation(() => ({
		mutationFn: (input: {
			oldAlbumId: string;
			newAlbumId: string;
		}): Promise<components['schemas']['RetagPairDismissResponse']> =>
			api.v3.POST(LibraryV3Api.dismissRetagPair(input.oldAlbumId, input.newAlbumId)),
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
