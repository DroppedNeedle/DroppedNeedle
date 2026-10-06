import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { PlaylistV3Api } from './PlaylistV3Api';
import { PlaylistQueryKeyFactory, type PlaylistV3UserId } from './PlaylistQueryKeyFactory';

export type PlaylistDetailV3 = components['schemas']['PlaylistDetail'];
export type PlaylistListItemV3 = components['schemas']['PlaylistListItem'];
export const isRedactedPlaylistV3 = (item: PlaylistListItemV3 | PlaylistDetailV3): boolean =>
	item.is_redacted === true;

// staleTime 0: the list must reflect creates/deletes/imports on every
// navigation (it can be mutated from elsewhere without an explicit
// invalidate). Stale-while-revalidate still renders the cached list instantly;
// the user-scoped key keeps per-account isolation.
export const getPlaylistListV3Query = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		staleTime: 0,
		queryKey: PlaylistQueryKeyFactory.list(authStore.user?.id),
		queryFn: async ({ signal }: { signal?: AbortSignal } = {}) => {
			const data = await api.global.v3.GET(PlaylistV3Api.list(), {
				signal
			});
			return data.playlists;
		}
	}));

export const getPlaylistDetailV3Query = (getId: Getter<string>, getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		staleTime: 0,
		refetchOnWindowFocus: false,
		queryKey: PlaylistQueryKeyFactory.detail(authStore.user?.id, getId()),
		queryFn: ({ signal }: { signal?: AbortSignal } = {}) =>
			api.global.v3.GET(PlaylistV3Api.detail(getId()), { signal }),
		retry: false
	}));

export type { PlaylistV3UserId };
