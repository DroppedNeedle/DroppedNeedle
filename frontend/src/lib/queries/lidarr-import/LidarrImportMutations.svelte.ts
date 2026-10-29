import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import { LIDARR_IMPORT_ENDPOINTS } from './endpoints';
import type { LidarrImportConnection } from './types';
import {
	anyFollowChanged,
	createAppMutation,
	homeChanged,
	lidarrCandidatesChanged,
	lidarrSettingsChanged
} from '$lib/queries/effects';

// The connection form renders errors inline, so save/test don't toast here.
export const saveLidarrConfigMutation = () =>
	createAppMutation(() => ({
		mutationFn: (connection: LidarrImportConnection) =>
			api.v3.PUT(LIDARR_IMPORT_ENDPOINTS.config(), connection),
		effects: () => [lidarrSettingsChanged()]
	}));

export const testLidarrMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (connection: LidarrImportConnection) =>
			api.v3.POST(LIDARR_IMPORT_ENDPOINTS.test(), connection)
	}));

export const importFromLidarrMutation = () =>
	createAppMutation(() => ({
		mutationFn: (selectedMbids: string[]) =>
			api.v3.POST(LIDARR_IMPORT_ENDPOINTS.import(), {
				selected_mbids: selectedMbids
			}),
		// Follows changed: the whole following prefix (artists list, new-release
		// views, the approvals count) and the candidates' already_following. Home
		// surfaces followed new releases, so it refreshes too.
		effects: () => [anyFollowChanged(), lidarrCandidatesChanged(authStore.user?.id), homeChanged()]
	}));
