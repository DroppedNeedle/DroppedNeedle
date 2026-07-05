import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import type { SpotifyPlaylistListResponse } from '$lib/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { createMutation, createQuery } from '@tanstack/svelte-query';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { PlaylistQueryKeyFactory } from '../playlists/PlaylistQueryKeyFactory';

const SPOTIFY_PLAYLISTS_KEY = (userId: string | undefined) => [
	'spotify-playlists',
	userId ?? 'anon'
];

export const getSpotifyPlaylistsQuery = () =>
	createQuery(() => ({
		staleTime: 5 * 60_000,
		gcTime: 10 * 60_000,
		refetchOnWindowFocus: false,
		queryKey: SPOTIFY_PLAYLISTS_KEY(authStore.user?.id),
		queryFn: () => api.global.get<SpotifyPlaylistListResponse>(API.me.spotifyPlaylists()),
		retry: false
	}));

interface ImportSpotifyPlaylistInput {
	id: string;
	name: string;
}

export const createImportSpotifyPlaylistMutation = () =>
	createMutation(() => ({
		mutationFn: (input: ImportSpotifyPlaylistInput) =>
			api.global.post<{ playlist_id: string }>(API.me.spotifyImport(input.id), {
				name: input.name
			}),
		onSuccess: () => {
			invalidateQueriesWithPersister({
				queryKey: PlaylistQueryKeyFactory.list(authStore.user?.id)
			});
			invalidateQueriesWithPersister({
				queryKey: SPOTIFY_PLAYLISTS_KEY(authStore.user?.id)
			});
		}
	}));
