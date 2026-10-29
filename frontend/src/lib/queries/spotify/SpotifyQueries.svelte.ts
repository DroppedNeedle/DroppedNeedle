import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { SPOTIFY_ENDPOINTS } from './endpoints';
import { SpotifyQueryKeyFactory } from './SpotifyQueryKeyFactory';
import { createAppMutation, playlistsChanged, spotifyPlaylistsChanged } from '$lib/queries/effects';

export const getSpotifyPlaylistsQuery = () =>
	createQuery(() => ({
		gcTime: 10 * 60_000,
		refetchOnWindowFocus: false,
		queryKey: SpotifyQueryKeyFactory.playlists(authStore.user?.id),
		queryFn: () => api.v3.GET(SPOTIFY_ENDPOINTS.playlists()),
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
			api.v3.POST(SPOTIFY_ENDPOINTS.importPlaylist(input.id), { name: input.name }),
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
				api.v3.GET(SPOTIFY_ENDPOINTS.redirectUri(), { signal })
		};
	});
