import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { createAppMutation, laterChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

export type LaterAlbum = components['schemas']['LaterAlbum'];
export type LaterRequest = components['schemas']['LaterRequest'];

/** The most albums the pile's one read holds: the server's page maximum. */
export const LATER_PAGE = 100;

/** The caller's Later pile, newest first, read while `getEnabled` holds. */
export const getLaterPileQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled() && Boolean(authStore.user?.id),
		queryKey: DiscoverQueryKeyFactory.v3.later(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/discover/later', { query: { limit: LATER_PAGE } }), { signal })
	}));

/** Whether one album is on the caller's Later pile: a page of that row or none. */
export const getLaterAlbumQuery = (getMbid: Getter<string | null>) =>
	createQuery(() => {
		const mbid = getMbid() ?? '';
		return {
			enabled: Boolean(mbid) && Boolean(authStore.user?.id),
			queryKey: DiscoverQueryKeyFactory.v3.laterAlbum(authStore.user?.id, mbid),
			queryFn: ({ signal }) =>
				api.v3.GET(
					v3('/api/v3/discover/later', { query: { limit: 1, release_group_mbid: mbid } }),
					{ signal }
				)
		};
	});

/** An album to put on Later; `quiet` when the caller says the failure itself. */
export type LaterMark = LaterRequest & { mbid: string; quiet?: boolean };

/** Puts one album on the caller's Later pile. */
export const createMarkLaterMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ mbid, quiet: _quiet, ...body }: LaterMark) =>
			api.v3.PUT(
				v3('/api/v3/discover/later/{release_group_mbid}', { path: { release_group_mbid: mbid } }),
				body
			),
		effects: () => [laterChanged(authStore.user?.id)],
		onError: (_error, { quiet }) => {
			if (!quiet)
				toastStore.show({ message: "Couldn't put that on your Later pile.", type: 'error' });
		}
	}));

/** Takes one album off the caller's Later pile. */
export const createRemoveLaterMutation = () =>
	createAppMutation(() => ({
		mutationFn: (mbid: string) =>
			api.v3.DELETE(
				v3('/api/v3/discover/later/{release_group_mbid}', { path: { release_group_mbid: mbid } })
			),
		effects: () => [laterChanged(authStore.user?.id)],
		onError: () =>
			toastStore.show({ message: "Couldn't take that off your Later pile.", type: 'error' })
	}));
