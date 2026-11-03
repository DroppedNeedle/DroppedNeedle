import { v3 } from '$lib/api/v3/endpoint';
import {
	albumHeaderChanged,
	albumRemovalsChanged,
	albumReread,
	applyEffects,
	catalogChanged,
	createAppMutation,
	wantedChanged
} from '$lib/queries/effects';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { libraryStore } from '$lib/stores/library';
import { LibraryQueryKeyFactory } from '$lib/features/library/queries/LibraryQueryKeyFactory';

export type AlbumRemoval = components['schemas']['AlbumRemovalView'];
export type PutBackAnswer = components['schemas']['PutBackView'];

// An album's removals whose files are still in the recycle bin, newest
// first. Curators only: the query stays off for everyone else.
export const getAlbumRemovalsQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => ({
		queryKey: LibraryQueryKeyFactory.catalog.albumRemovals(authStore.user?.id, getAlbumId()),
		enabled: authStore.isTrusted && !!getAlbumId(),
		queryFn: async ({ signal }) => {
			const answer = await api.v3.GET(
				v3('/api/v3/library/albums/{album_id}/removals', { path: { album_id: getAlbumId() } }),
				{
					signal
				}
			);
			return answer.items;
		}
	}));

/** Read the album's removals again, after a removal or a put-back. */
export function invalidateAlbumRemovals(): Promise<void> {
	return applyEffects(albumRemovalsChanged(authStore.user?.id));
}

// Put one removal's files back. The album's tracks are listed again, so
// every catalog list refreshes, and so does the album page for its release
// group. A refusal reads the removals again, since one is
// usually stale.
export function createPutBackRemovalMutation() {
	return createAppMutation(() => ({
		mutationFn: ({
			albumId,
			removalId
		}: {
			albumId: string;
			removalId: string;
			releaseGroupMbid?: string | null;
		}): Promise<PutBackAnswer> =>
			api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/removals/{removal_id}/put-back', {
					path: { album_id: albumId, removal_id: removalId }
				})
			),
		onSuccess: (_answer, { releaseGroupMbid }) => {
			if (releaseGroupMbid) libraryStore.addMbid(releaseGroupMbid);
		},
		effects: (_answer, { releaseGroupMbid }) => {
			const userId = authStore.user?.id;
			const albumPage = releaseGroupMbid
				? [albumHeaderChanged(userId, releaseGroupMbid), albumReread(userId, releaseGroupMbid)]
				: [];
			return [catalogChanged(userId), wantedChanged(userId), ...albumPage];
		},
		onError: () => invalidateAlbumRemovals()
	}));
}
