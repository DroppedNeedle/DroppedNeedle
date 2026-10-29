import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { PlaylistV3Api } from './PlaylistV3Api';
import { PlaylistQueryKeyFactory, type PlaylistV3UserId } from './PlaylistQueryKeyFactory';
import { STALE_LIVE, STALE_SLOW } from '$lib/queries/staleTime';

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
		staleTime: STALE_LIVE,
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
		staleTime: STALE_LIVE,
		refetchOnWindowFocus: false,
		queryKey: PlaylistQueryKeyFactory.detail(authStore.user?.id, getId()),
		queryFn: ({ signal }: { signal?: AbortSignal } = {}) =>
			api.global.v3.GET(PlaylistV3Api.detail(getId()), { signal }),
		retry: false
	}));

export type { PlaylistV3UserId };

// Resolving looks up where each track can play and stores what it heals on the
// server, so one answer serves repeat visits for the slow tier.
export const getPlaylistSourcesV3QueryOptions = (userId: PlaylistV3UserId, playlistId: string) =>
	queryOptions({
		queryKey: PlaylistQueryKeyFactory.sources(userId, playlistId),
		queryFn: async ({ signal }) =>
			(await api.global.v3.POST(PlaylistV3Api.resolveSources(playlistId), undefined, { signal }))
				.sources,
		staleTime: STALE_SLOW
	});
