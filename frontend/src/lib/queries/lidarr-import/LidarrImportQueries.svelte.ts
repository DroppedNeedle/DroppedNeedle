import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';

import { LidarrImportQueryKeyFactory } from './LidarrImportQueryKeyFactory';
import type { LidarrArtistList, LidarrImportConnection, LidarrImportStatus } from './types';

type Getter<T> = () => T;

// Admin-only: the masked connection settings for the Settings card.
export const getLidarrImportConfigQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: LidarrImportQueryKeyFactory.config(),
		queryFn: ({ signal }) =>
			api.global.get<LidarrImportConnection>(API.lidarrImport.config(), { signal }),
		enabled: getEnabled()
	}));

// Any user: the non-admin gate for the import button. Returns only { configured } - never
// the url/api_key (config-leak guard), so this is safe for non-admins to read.
export const getLidarrImportStatusQuery = () =>
	createQuery(() => ({
		queryKey: LidarrImportQueryKeyFactory.status(),
		queryFn: ({ signal }) =>
			api.global.get<LidarrImportStatus>(API.lidarrImport.status(), { signal })
	}));

// Any user: the monitored-artist candidates, annotated for the requesting user. Fetched
// only while the modal is open.
export const getLidarrImportCandidatesQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		queryKey: LidarrImportQueryKeyFactory.candidates(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.get<LidarrArtistList>(API.lidarrImport.artists(), { signal }),
		enabled: getEnabled()
	}));
