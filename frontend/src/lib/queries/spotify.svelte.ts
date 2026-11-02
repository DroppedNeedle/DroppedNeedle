import { SpotifyQueryKeyFactory } from '$lib/queries/featureKeys';

import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { createAppMutation, playlistsChanged, spotifyPlaylistsChanged } from '$lib/queries/effects';

export const getSpotifyPlaylistsQuery = () =>
	createQuery(() => ({
		gcTime: 10 * 60_000,
		refetchOnWindowFocus: false,
		queryKey: SpotifyQueryKeyFactory.playlists(authStore.user?.id),
		queryFn: () => api.v3.GET(v3('/api/v3/acquire/spotify/playlists')),
		retry: false
	}));

interface ImportSpotifyPlaylistInput {
	id: string;
	name: string;
}

export const createImportSpotifyPlaylistMutation = () =>
	createAppMutation(() => ({
		// The name seeds the internal playlist record the import fills.
		mutationFn: (input: ImportSpotifyPlaylistInput) =>
			api.v3.POST(v3('/api/v3/acquire/spotify/playlists/{id}/import', { path: { id: input.id } }), {
				name: input.name
			}),
		effects: () => [
			playlistsChanged(authStore.user?.id),
			spotifyPlaylistsChanged(authStore.user?.id)
		]
	}));

/** The address Spotify sends the browser back to, for the saved redirect origin. */
export const getSpotifyRedirectUriQuery = (getOrigin: Getter<string | undefined>) =>
	createQuery(() => {
		const origin = getOrigin();
		return {
			enabled: origin !== undefined,
			queryKey: SpotifyQueryKeyFactory.redirectUri(authStore.user?.id, origin ?? null),
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.v3.GET(v3('/api/v3/acquire/spotify/redirect-uri'), { signal })
		};
	});
