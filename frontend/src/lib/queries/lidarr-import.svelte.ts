import { LidarrImportQueryKeyFactory } from '$lib/queries/featureKeys';
import type { components } from '$lib/api/v3/openapi';

import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import {
	anyFollowChanged,
	createAppMutation,
	homeChanged,
	lidarrCandidatesChanged,
	lidarrSettingsChanged
} from '$lib/queries/effects';

export type LidarrImportConnection = components['schemas']['LidarrConnectionSettings'];
export type LidarrImportResult = components['schemas']['LidarrImportResponse'];

type Getter<T> = () => T;

// Admin-only: the masked connection settings for the Settings card.
export const getLidarrImportConfigQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		queryKey: LidarrImportQueryKeyFactory.config(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/acquire/lidarr-import/config'), { signal }),
		enabled: getEnabled()
	}));

// Admin-only: the monitored-artist candidates, annotated for the requesting admin. Fetched
// only while the sync card is unlocked.
export const getLidarrImportCandidatesQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		queryKey: LidarrImportQueryKeyFactory.candidates(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/acquire/lidarr-import/artists'), { signal }),
		enabled: getEnabled()
	}));

// The connection form renders errors inline, so save/test don't toast here.
export const saveLidarrConfigMutation = () =>
	createAppMutation(() => ({
		mutationFn: (connection: LidarrImportConnection) =>
			api.v3.PUT(v3('/api/v3/acquire/lidarr-import/config'), connection),
		effects: () => [lidarrSettingsChanged()]
	}));

export const testLidarrMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (connection: LidarrImportConnection) =>
			api.v3.POST(v3('/api/v3/acquire/lidarr-import/test'), connection)
	}));

export const importFromLidarrMutation = () =>
	createAppMutation(() => ({
		mutationFn: (selectedMbids: string[]) =>
			api.v3.POST(v3('/api/v3/acquire/lidarr-import/import'), {
				selected_mbids: selectedMbids
			}),
		// Follows changed: the whole following prefix (artists list, new-release
		// views, the approvals count) and the candidates' already_following. Home
		// surfaces followed new releases, so it refreshes too.
		effects: () => [anyFollowChanged(), lidarrCandidatesChanged(authStore.user?.id), homeChanged()]
	}));
