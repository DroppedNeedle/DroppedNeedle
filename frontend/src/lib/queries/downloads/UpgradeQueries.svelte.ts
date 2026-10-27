import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_UPGRADE_ENDPOINTS } from './endpoints';

// Quality upgrades (CollectionManagement Feature B). Admin/trusted-only surfaces:
// the Requests "Upgrades" worklist + the album page's per-track/album affordances.

export const getCutoffUnmetQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled(),
		queryKey: DownloadQueryKeyFactory.cutoffUnmet(),
		queryFn: ({ signal }) => api.global.v3.GET(DOWNLOAD_UPGRADE_ENDPOINTS.cutoffUnmet(), { signal })
	}));

async function invalidateAfterUpgrade() {
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.cutoffUnmet() });
	await invalidateQueriesWithPersister({
		queryKey: DownloadQueryKeyFactory.tasks(authStore.user?.id)
	});
}

export type UpgradeAlbumInput = components['schemas']['UpgradeAlbumRequest'];

export function requestUpgradeAlbum() {
	return createMutation(() => ({
		mutationFn: (input: UpgradeAlbumInput) =>
			api.global.v3.POST(DOWNLOAD_UPGRADE_ENDPOINTS.album(), input),
		onSuccess: invalidateAfterUpgrade
	}));
}

export type UpgradeTrackInput = components['schemas']['UpgradeTrackRequest'];

export function requestUpgradeTrack() {
	return createMutation(() => ({
		mutationFn: (input: UpgradeTrackInput) =>
			api.global.v3.POST(DOWNLOAD_UPGRADE_ENDPOINTS.track(), input),
		onSuccess: invalidateAfterUpgrade
	}));
}
