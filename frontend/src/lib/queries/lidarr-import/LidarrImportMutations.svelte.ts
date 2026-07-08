import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { FollowQueryKeyFactory } from '$lib/queries/following/FollowQueryKeyFactory';
import { notifyPendingApprovalCountChanged } from '$lib/utils/requestsApi';

import { LidarrImportQueryKeyFactory } from './LidarrImportQueryKeyFactory';
import type { LidarrImportConnection, LidarrImportResult, LidarrTestResult } from './types';

// The connection form renders errors inline, so save/test don't toast here.
export const saveLidarrConfigMutation = () =>
	createMutation(() => ({
		mutationFn: (connection: LidarrImportConnection) =>
			api.global.put<LidarrImportConnection>(API.lidarrImport.config(), connection),
		onSuccess: async () => {
			await invalidateQueriesWithPersister({ queryKey: LidarrImportQueryKeyFactory.config() });
			await invalidateQueriesWithPersister({ queryKey: LidarrImportQueryKeyFactory.status() });
		}
	}));

export const testLidarrMutation = () =>
	createMutation(() => ({
		mutationFn: (connection: LidarrImportConnection) =>
			api.global.post<LidarrTestResult>(API.lidarrImport.test(), connection)
	}));

export const importFromLidarrMutation = () =>
	createMutation(() => ({
		mutationFn: (selectedMbids: string[]) =>
			api.global.post<LidarrImportResult>(API.lidarrImport.import(), {
				selected_mbids: selectedMbids
			}),
		onSuccess: async () => {
			// Follows changed: sweep the whole following prefix (artists list + new-release
			// views) and re-annotate the candidate list's already_following. Home surfaces
			// followed new releases, so invalidate it too (cross-domain rule).
			await invalidateQueriesWithPersister({
				queryKey: FollowQueryKeyFactory.followingPrefix
			});
			await invalidateQueriesWithPersister({
				queryKey: LidarrImportQueryKeyFactory.candidates(authStore.user?.id)
			});
			await invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix });
			// An admin importer's own auto-download is live immediately; a non-admin mints a
			// pending batch. Either way the pending-approval badge may need refreshing.
			notifyPendingApprovalCountChanged();
		}
	}));
