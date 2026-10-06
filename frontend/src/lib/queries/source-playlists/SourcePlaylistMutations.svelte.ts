import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { PlaylistQueryKeyFactory } from '$lib/queries/playlists/PlaylistQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
import { RemoteQueryKeyFactory } from '$lib/queries/remotes/RemoteQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';
import type { SourcePlaylistSource } from '$lib/types';

import { SourcePlaylistQueryKeyFactory } from './SourcePlaylistQueryKeyFactory';

// Copies one remote playlist into the caller's playlists. The new playlist
// shows up in the playlist list, so the playlist root is refreshed with the
// source's own lists.
export const createSourcePlaylistImportMutation = (getSource: () => SourcePlaylistSource) =>
	createMutation(() => ({
		mutationFn: (playlistId: string) =>
			api.global.v3.POST(REMOTE_ENDPOINTS.importPlaylist(getSource(), playlistId)),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (_data, _playlistId, context) => {
			// A late answer after an account switch must not touch the new
			// user's cache.
			const userId = context.userId;
			if (!userId || authStore.user?.id !== userId) return;
			const source = getSource();
			await Promise.all([
				invalidateQueriesWithPersister({
					queryKey: SourcePlaylistQueryKeyFactory.source(userId, source)
				}),
				invalidateQueriesWithPersister({
					queryKey: RemoteQueryKeyFactory.source(userId, source)
				}),
				invalidateQueriesWithPersister({ queryKey: PlaylistQueryKeyFactory.root(userId) }),
				invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix }),
				invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all })
			]);
		}
	}));
