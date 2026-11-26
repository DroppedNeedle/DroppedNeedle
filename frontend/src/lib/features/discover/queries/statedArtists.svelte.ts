import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import type { components } from '$lib/api/v3/openapi';
import { createAppMutation, statedArtistsChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

export type StatedArtist = components['schemas']['StatedArtist'];

/** The artists the caller named as ones they love, read while `getEnabled` holds. */
export const getStatedArtistsQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		queryKey: DiscoverQueryKeyFactory.v3.statedArtists(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/discover/stated-artists'), { signal })
	}));

/** An artist to name, with the name as shown. */
export interface StatedArtistName {
	mbid: string;
	name: string;
}

/** Names one artist the caller loves. */
export const createNameStatedArtistMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ mbid, name }: StatedArtistName) =>
			api.v3.PUT(
				v3('/api/v3/discover/stated-artists/{artist_mbid}', { path: { artist_mbid: mbid } }),
				{ artist_name: name }
			),
		effects: () => [statedArtistsChanged(authStore.user?.id)],
		onError: (error) =>
			toastStore.show({
				// A refusal says why, such as a full list; anything else stays generic.
				message:
					error instanceof ApiError && error.status < 500
						? error.message
						: "Couldn't add that artist.",
				type: 'error'
			})
	}));

/** Takes one artist off the caller's list. */
export const createRemoveStatedArtistMutation = () =>
	createAppMutation(() => ({
		mutationFn: (mbid: string) =>
			api.v3.DELETE(
				v3('/api/v3/discover/stated-artists/{artist_mbid}', { path: { artist_mbid: mbid } })
			),
		effects: () => [statedArtistsChanged(authStore.user?.id)],
		onError: () => toastStore.show({ message: "Couldn't remove that artist.", type: 'error' })
	}));
