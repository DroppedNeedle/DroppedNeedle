import { v3 } from '$lib/api/v3/endpoint';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { PlaylistQueryKeyFactory, type PlaylistV3UserId } from './PlaylistQueryKeyFactory';
import { STALE_LIVE, STALE_SLOW } from '$lib/queries/staleTime';
import { readNow } from '$lib/queries/readNow';

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
			const data = await api.v3.GET(v3('/api/v3/playlists'), {
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
			api.v3.GET(v3('/api/v3/playlists/{playlist_id}', { path: { playlist_id: getId() } }), {
				signal
			}),
		retry: false
	}));

export type { PlaylistV3UserId };

export type PlaylistMemberV3 = components['schemas']['PlaylistMember'];

/** The people the caller's playlist is shared with. */
export const getPlaylistMembersV3Query = (getId: Getter<string>, getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		staleTime: STALE_LIVE,
		queryKey: PlaylistQueryKeyFactory.members(authStore.user?.id, getId()),
		queryFn: async ({ signal }: { signal?: AbortSignal } = {}) =>
			(
				await api.v3.GET(
					v3('/api/v3/playlists/{playlist_id}/members', { path: { playlist_id: getId() } }),
					{ signal }
				)
			).members
	}));

/** Everyone a playlist can be shared with, by display name. */
export const getUserDirectoryV3Query = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		staleTime: STALE_SLOW,
		queryKey: PlaylistQueryKeyFactory.people(authStore.user?.id),
		queryFn: async ({ signal }: { signal?: AbortSignal } = {}) =>
			(await api.v3.GET(v3('/api/v3/users/directory'), { signal })).users
	}));

// Resolving looks up where each track can play and stores what it heals on the
// server, so one answer serves repeat visits for the slow tier.
export const getPlaylistSourcesV3QueryOptions = (userId: PlaylistV3UserId, playlistId: string) =>
	queryOptions({
		queryKey: PlaylistQueryKeyFactory.sources(userId, playlistId),
		queryFn: async ({ signal }) =>
			(
				await api.v3.POST(
					v3('/api/v3/playlists/{playlist_id}/resolve-sources', {
						path: { playlist_id: playlistId }
					}),
					undefined,
					{ signal }
				)
			).sources,
		staleTime: STALE_SLOW
	});

/** The caller's playlists as they are now, for the add-to-playlist dialog. */
export const fetchPlaylistListV3 = () =>
	readNow(PlaylistQueryKeyFactory.list(authStore.user?.id), async (signal) => {
		const data = await api.v3.GET(v3('/api/v3/playlists'), { signal });
		return data.playlists;
	});
