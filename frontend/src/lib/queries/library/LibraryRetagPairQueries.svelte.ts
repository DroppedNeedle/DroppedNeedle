import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { toastStore } from '$lib/stores/toast';
import { invalidateLibraryCatalog } from './LibraryCatalogInvalidation';
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
			api.global.v3.GET(LibraryV3Api.retagPairs(), { signal })
	}));

const errorMessage = (error: unknown, fallback: string) =>
	error instanceof Error && error.message ? error.message : fallback;

// A join merges the old album into the new one, so the pair list, both
// albums and every album list change; the library prefix covers them all.
export function joinRetagPair() {
	return createMutation(() => ({
		mutationFn: (
			input: RetagPairJoinInput
		): Promise<components['schemas']['RetagPairJoinResponse']> =>
			api.global.v3.POST(LibraryV3Api.joinRetagPair(input.oldAlbumId, input.newAlbumId), {
				keep_edition_of: input.keepEditionOf ?? null,
				expected_identity_revisions: input.expectedIdentityRevisions
			}),
		onSuccess: async () => {
			await invalidateLibraryCatalog();
			toastStore.show({ message: 'Albums joined', type: 'success' });
		},
		onError: (error) =>
			toastStore.show({ message: errorMessage(error, 'Could not join the albums'), type: 'error' })
	}));
}

export function dismissRetagPair() {
	return createMutation(() => ({
		mutationFn: (input: {
			oldAlbumId: string;
			newAlbumId: string;
		}): Promise<components['schemas']['RetagPairDismissResponse']> =>
			api.global.v3.POST(LibraryV3Api.dismissRetagPair(input.oldAlbumId, input.newAlbumId)),
		onSuccess: invalidateLibraryCatalog,
		onError: (error) =>
			toastStore.show({
				message: errorMessage(error, 'Could not keep the albums apart'),
				type: 'error'
			})
	}));
}
