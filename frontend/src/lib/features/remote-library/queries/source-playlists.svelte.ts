import { SourcePlaylistQueryKeyFactory } from '$lib/queries/featureKeys';
import type { RemotePlaylist, RemoteSource } from '$lib/features/remote-library/queries/types';

import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import {
	anyLibraryChanged,
	createAppMutation,
	homeChanged,
	playlistsChanged,
	remoteSourceChanged,
	sourcePlaylistsChanged
} from '$lib/queries/effects';

type Getter<T> = () => T;

/** A source's playlists with the account they are read as. */
export type SourcePlaylistCollection = {
	account_mode: 'linked' | 'shared';
	account_label: string;
	playlists: RemotePlaylist[];
};

// The playlist list and the account it belongs to come from two v3 reads.
async function fetchCollection(
	source: RemoteSource,
	signal: AbortSignal
): Promise<SourcePlaylistCollection> {
	const [playlists, connection] = await Promise.all([
		api.v3.GET(v3('/api/v3/remotes/{source}/playlists', { path: { source } }), { signal }),
		api.v3.GET(v3('/api/v3/remotes/{source}/connection', { path: { source } }), { signal })
	]);
	return {
		account_mode: connection.account_mode === 'linked' ? 'linked' : 'shared',
		account_label: connection.account_label,
		playlists: playlists.items
	};
}

function fetchDetail(
	source: RemoteSource,
	playlistId: string,
	signal: AbortSignal
): Promise<components['schemas']['RemotesPlaylistDetail']> {
	return api.v3.GET(
		v3('/api/v3/remotes/{source}/playlists/{id}', { path: { source, id: playlistId } }),
		{ signal }
	);
}

export const getSourcePlaylistsQuery = (
	getSource: Getter<RemoteSource>,
	getLimit: Getter<number> = () => 200,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		queryKey: SourcePlaylistQueryKeyFactory.list(authStore.user?.id, getSource(), getLimit()),
		queryFn: ({ signal }) => fetchCollection(getSource(), signal),
		enabled: getEnabled() && !!authStore.user?.id
	}));

export const getSourcePlaylistDetailQuery = (
	getSource: Getter<RemoteSource>,
	getPlaylistId: Getter<string>
) =>
	createQuery(() => ({
		queryKey: SourcePlaylistQueryKeyFactory.detail(
			authStore.user?.id,
			getSource(),
			getPlaylistId()
		),
		queryFn: ({ signal }) => fetchDetail(getSource(), getPlaylistId(), signal),
		enabled: !!authStore.user?.id && !!getPlaylistId()
	}));

// Copies one remote playlist into the caller's playlists. The new playlist
// shows up in the playlist list, so the playlist root is refreshed with the
// source's own lists.
export const createSourcePlaylistImportMutation = (getSource: () => RemoteSource) =>
	createAppMutation(() => ({
		mutationFn: (playlistId: string) =>
			api.v3.POST(
				v3('/api/v3/remotes/{source}/playlists/{id}/import', {
					path: { source: getSource(), id: playlistId }
				})
			),
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
