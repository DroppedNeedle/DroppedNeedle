import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { createMutation, createQuery } from '@tanstack/svelte-query';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { PlaylistQueryKeyFactory } from '../playlists/PlaylistQueryKeyFactory';
import { userIdSegment } from '../userKeySegment';
import { SPOTIFY_ENDPOINTS } from './endpoints';

const SPOTIFY_PLAYLISTS_KEY = (userId: string | null | undefined) => [
	'spotify-playlists',
	userIdSegment(userId)
];

export const getSpotifyPlaylistsQuery = () =>
	createQuery(() => ({
		staleTime: 5 * 60_000,
		gcTime: 10 * 60_000,
		refetchOnWindowFocus: false,
		queryKey: SPOTIFY_PLAYLISTS_KEY(authStore.user?.id),
		queryFn: () => api.global.v3.GET(SPOTIFY_ENDPOINTS.playlists()),
		retry: false
	}));

interface ImportSpotifyPlaylistInput {
	id: string;
	name: string;
}

export const createImportSpotifyPlaylistMutation = () =>
	createMutation(() => ({
		// v3 takes no import body (the name rides the stored Spotify
		// playlist); the input keeps `name` so callers stay unchanged.
		mutationFn: (input: ImportSpotifyPlaylistInput) =>
			api.global.v3.POST(SPOTIFY_ENDPOINTS.importPlaylist(input.id)),
		onSuccess: () => {
			invalidateQueriesWithPersister({
				queryKey: PlaylistQueryKeyFactory.root(authStore.user?.id)
			});
			invalidateQueriesWithPersister({
				queryKey: SPOTIFY_PLAYLISTS_KEY(authStore.user?.id)
			});
		}
	}));
