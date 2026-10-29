import { api } from '$lib/api/client';
import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
import { authStore } from '$lib/stores/authStore.svelte';
import type { SourcePlaylistSource } from '$lib/types';
import {
	anyLibraryChanged,
	createAppMutation,
	homeChanged,
	playlistsChanged,
	remoteSourceChanged,
	sourcePlaylistsChanged
} from '$lib/queries/effects';

// Copies one remote playlist into the caller's playlists. The new playlist
// shows up in the playlist list, so the playlist root is refreshed with the
// source's own lists.
export const createSourcePlaylistImportMutation = (getSource: () => SourcePlaylistSource) =>
	createAppMutation(() => ({
		mutationFn: (playlistId: string) =>
			api.v3.POST(REMOTE_ENDPOINTS.importPlaylist(getSource(), playlistId)),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, _playlistId, context) => {
			// A late answer after an account switch must not touch the new
			// user's cache.
			const userId = context.userId;
			if (!userId || authStore.user?.id !== userId) return [];
			const source = getSource();
			return [
				sourcePlaylistsChanged(userId, source),
				remoteSourceChanged(userId, source),
				playlistsChanged(userId),
				homeChanged(),
				anyLibraryChanged()
			];
		}
	}));
