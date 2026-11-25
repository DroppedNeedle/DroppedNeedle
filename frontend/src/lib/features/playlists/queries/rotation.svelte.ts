import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { v3 } from '$lib/api/v3/endpoint';
import { authStore } from '$lib/stores/authStore.svelte';
import { STALE_LIVE } from '$lib/queries/staleTime';
import { createAppMutation, playlistChanged, playlistRotationChanged } from '$lib/queries/effects';
import { PlaylistQueryKeyFactory } from './PlaylistQueryKeyFactory';

export type RotationStatusV3 = components['schemas']['RotationStatus'];

/** The caller's lists on rotation: the switch, the next change, what waits. */
export const getRotationQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		staleTime: STALE_LIVE,
		queryKey: PlaylistQueryKeyFactory.rotation(authStore.user?.id),
		queryFn: ({ signal }: { signal?: AbortSignal } = {}) =>
			api.v3.GET(v3('/api/v3/me/rotation'), { signal })
	}));

/** Turns the caller's lists on rotation on or off. Off deletes them. */
export const createSetRotationMutation = () =>
	createAppMutation(() => ({
		mutationFn: (enabled: boolean) => api.v3.PUT(v3('/api/v3/me/rotation'), { enabled }),
		effects: () => [playlistRotationChanged(authStore.user?.id)]
	}));

/** Keeps one list on rotation past Monday, or lets it go. */
export const createPinRotationMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { playlistId: string; pinned: boolean }) =>
			api.v3.PUT(
				v3('/api/v3/playlists/{playlist_id}/rotation', { path: { playlist_id: vars.playlistId } }),
				{ pinned: vars.pinned }
			),
		effects: (_data, vars) => [
			playlistRotationChanged(authStore.user?.id),
			playlistChanged(authStore.user?.id, vars.playlistId)
		]
	}));

/** Saves one list on rotation as an ordinary playlist of the caller's. */
export const createCopyRotationMutation = () =>
	createAppMutation(() => ({
		mutationFn: (playlistId: string) =>
			api.v3.POST(
				v3('/api/v3/playlists/{playlist_id}/rotation/copy', {
					path: { playlist_id: playlistId }
				})
			),
		effects: () => [playlistRotationChanged(authStore.user?.id)]
	}));
