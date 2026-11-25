import { createQuery, useQueryClient } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { createAppMutation, smartPlaylistChanged } from '$lib/queries/effects';
import { STALE_LIVE } from '$lib/queries/staleTime';
import { authStore } from '$lib/stores/authStore.svelte';
import { PlaylistQueryKeyFactory as keys } from './PlaylistQueryKeyFactory';

export type SmartPlaylist = components['schemas']['SmartPlaylist'];
type SmartPlaylistInput = components['schemas']['SmartPlaylistInput'];
const userId = () => authStore.user?.id;

export const getSmartPlaylistsQuery = () =>
	createQuery(() => ({
		queryKey: keys.smartList(userId()),
		enabled: Boolean(userId()),
		staleTime: STALE_LIVE,
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/smart-playlists'), { signal })
	}));

export const getSmartPlaylistQuery = (getId: Getter<string>) =>
	createQuery(() => {
		const id = getId();
		return {
			queryKey: keys.smartDetail(userId(), id),
			enabled: Boolean(userId()),
			staleTime: STALE_LIVE,
			queryFn: ({ signal }) =>
				api.v3.GET(v3('/api/v3/smart-playlists/{playlist_id}', { path: { playlist_id: id } }), {
					signal
				})
		};
	});

export const getSmartTracksQuery = (getId: Getter<string>, getAfter: Getter<string | null>) =>
	createQuery(() => {
		const id = getId();
		const after = getAfter();
		return {
			queryKey: keys.smartTracksPage(userId(), id, after),
			enabled: Boolean(userId()),
			staleTime: STALE_LIVE,
			queryFn: ({ signal }) =>
				api.v3.GET(
					v3('/api/v3/smart-playlists/{playlist_id}/tracks', {
						path: { playlist_id: id },
						query: { after: after ?? undefined, limit: 50 }
					}),
					{ signal }
				)
		};
	});

export const createSaveSmartPlaylistMutation = () =>
	createAppMutation(
		() => ({
			mutationFn: ({ id, input }: { id?: string; userId: string; input: SmartPlaylistInput }) =>
				id
					? api.v3.PUT(
							v3('/api/v3/smart-playlists/{playlist_id}', { path: { playlist_id: id } }),
							input
						)
					: api.v3.POST(v3('/api/v3/smart-playlists'), input),
			effects: (saved, variables) => [smartPlaylistChanged(variables.userId, saved.id)]
		}),
		useQueryClient()
	);
