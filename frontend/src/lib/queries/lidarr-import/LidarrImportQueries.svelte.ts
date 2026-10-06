import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import { LidarrImportQueryKeyFactory } from './LidarrImportQueryKeyFactory';
import { LIDARR_IMPORT_ENDPOINTS } from './endpoints';

type Getter<T> = () => T;

// Admin-only: the masked connection settings for the Settings card.
export const getLidarrImportConfigQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: LidarrImportQueryKeyFactory.config(),
		queryFn: ({ signal }) => api.global.v3.GET(LIDARR_IMPORT_ENDPOINTS.config(), { signal }),
		enabled: getEnabled()
	}));

// Admin-only: the monitored-artist candidates, annotated for the requesting admin. Fetched
// only while the sync card is unlocked.
export const getLidarrImportCandidatesQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		queryKey: LidarrImportQueryKeyFactory.candidates(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(LIDARR_IMPORT_ENDPOINTS.artists(), { signal }),
		enabled: getEnabled()
	}));
