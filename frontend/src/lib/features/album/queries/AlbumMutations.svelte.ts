import { v3 } from '$lib/api/v3/endpoint';
import { useQueryClient } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { AlbumBasicInfo } from '$lib/types';
import {
	albumHeaderChanged,
	albumReread,
	applyEffectsIn,
	createAppMutation
} from '$lib/queries/effects';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { AlbumQueryKeyFactory } from './AlbumQueryKeyFactory';

type RefreshTarget = { userId: string | null | undefined; albumId: string };

// Read the album from MusicBrainz again. The answer is the fresh header; the
// tracklist, the sources' copies and the library status are read again
// after it, whether or not the refresh went through.
export function createRefreshAlbumMutation() {
	const client = useQueryClient();
	return createAppMutation(
		() => ({
			mutationFn: ({ albumId }: RefreshTarget): Promise<AlbumBasicInfo> =>
				api.v3.POST(v3('/api/v3/albums/{album_id}/refresh', { path: { album_id: albumId } })),
			onSuccess: (album, { userId, albumId }) =>
				setQueryDataWithPersister(
					AlbumQueryKeyFactory.basic(userId, albumId),
					album,
					undefined,
					client
				),
			effects: (_album, { userId, albumId }) => [albumReread(userId, albumId)],
			onError: (_error, { userId, albumId }) =>
				applyEffectsIn(client, [albumReread(userId, albumId), albumHeaderChanged(userId, albumId)])
		}),
		client
	);
}
