import { createMutation, useQueryClient } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { AlbumBasicInfo } from '$lib/types';
import { extractServiceStatus } from '$lib/utils/serviceStatus';
import { AlbumMatchKeyFactory } from '../albumMatch';
import { LibraryQueryKeyFactory } from '../library/LibraryQueryKeyFactory';
import { invalidateQueriesWithPersister, setQueryDataWithPersister } from '../QueryClient';
import { AlbumQueryKeyFactory } from './AlbumQueryKeyFactory';
import { CATALOG_ENDPOINTS } from './endpoints';

type RefreshTarget = { userId: string | null | undefined; albumId: string };

// Read the album from MusicBrainz again. The answer is the fresh header; the
// tracklist, the sources' copies and the library status are read again
// after it, whether or not the refresh went through.
export function refreshAlbum() {
	const client = useQueryClient();
	return createMutation(() => ({
		mutationFn: async ({ albumId }: RefreshTarget): Promise<AlbumBasicInfo> => {
			const album = await api.global.v3.POST(CATALOG_ENDPOINTS.albumRefresh(albumId));
			extractServiceStatus(album);
			return album;
		},
		onSuccess: (album, { userId, albumId }) =>
			setQueryDataWithPersister(
				AlbumQueryKeyFactory.basic(userId, albumId),
				album,
				undefined,
				client
			),
		onSettled: async (_album, error, { userId, albumId }) => {
			const keys: (readonly unknown[])[] = [
				AlbumQueryKeyFactory.tracks(userId, albumId),
				AlbumMatchKeyFactory.album(userId, albumId),
				LibraryQueryKeyFactory.album(albumId)
			];
			if (error) keys.push(AlbumQueryKeyFactory.basic(userId, albumId));
			await Promise.allSettled(
				keys.map((queryKey) => invalidateQueriesWithPersister({ queryKey }, undefined, { client }))
			);
		}
	}));
}
