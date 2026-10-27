import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { WantedQueryKeyFactory } from '$lib/queries/wanted/WantedQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';
import { libraryStore } from '$lib/stores/library';
import { AlbumMatchKeyFactory } from '$lib/queries/albumMatch';
import { AlbumQueryKeyFactory } from '$lib/queries/catalog/AlbumQueryKeyFactory';
import { invalidateLibraryCatalog } from './LibraryCatalogInvalidation';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';

export type AlbumRemoval = components['schemas']['AlbumRemovalView'];
export type PutBackAnswer = components['schemas']['PutBackView'];

// An album's removals whose files are still in the recycle bin, newest
// first. Curators only: the query stays off for everyone else.
export const getAlbumRemovalsQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => ({
		queryKey: LibraryQueryKeyFactory.catalog.albumRemovals(authStore.user?.id, getAlbumId()),
		enabled: authStore.isTrusted && !!getAlbumId(),
		queryFn: async ({ signal }) => {
			const answer = await api.global.v3.GET(LibraryV3Api.albumRemovals(getAlbumId()), {
				signal
			});
			return answer.items;
		}
	}));

/** Read the album's removals again, after a removal or a put-back. */
export function invalidateAlbumRemovals(): Promise<void> {
	return invalidateQueriesWithPersister({
		queryKey: LibraryQueryKeyFactory.catalog.albumRemovalsPrefix(authStore.user?.id)
	});
}

// Put one removal's files back. The album's tracks are listed again, so
// every catalog list refreshes, and so does the album page for its release
// group. A refusal reads the removals again, since one is
// usually stale.
export function putBackRemoval() {
	return createMutation(() => ({
		mutationFn: ({
			albumId,
			removalId
		}: {
			albumId: string;
			removalId: string;
			releaseGroupMbid?: string | null;
		}): Promise<PutBackAnswer> =>
			api.global.v3.POST(LibraryV3Api.putBackRemoval(albumId, removalId)),
		onSuccess: async (_answer, { releaseGroupMbid }) => {
			const userId = authStore.user?.id;
			const albumPage = releaseGroupMbid
				? [
						AlbumQueryKeyFactory.basic(userId, releaseGroupMbid),
						AlbumQueryKeyFactory.tracks(userId, releaseGroupMbid),
						AlbumMatchKeyFactory.album(userId, releaseGroupMbid)
					]
				: [];
			if (releaseGroupMbid) libraryStore.addMbid(releaseGroupMbid);
			await Promise.allSettled([
				invalidateLibraryCatalog(),
				...[WantedQueryKeyFactory.prefix, ...albumPage].map((queryKey) =>
					invalidateQueriesWithPersister({ queryKey })
				)
			]);
		},
		onError: () => invalidateAlbumRemovals()
	}));
}
