import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_UPGRADE_ENDPOINTS } from './endpoints';
import { createAppMutation, downloadsChanged, upgradesChanged } from '$lib/queries/effects';

// Quality upgrades (CollectionManagement Feature B). Admin/trusted-only surfaces:
// the Requests "Upgrades" worklist + the album page's per-track/album affordances.

export const getCutoffUnmetQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled(),
		queryKey: DownloadQueryKeyFactory.cutoffUnmet(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_UPGRADE_ENDPOINTS.cutoffUnmet(), { signal })
	}));

const afterUpgrade = () => [upgradesChanged(), downloadsChanged(authStore.user?.id)];

export type UpgradeAlbumInput = components['schemas']['UpgradeAlbumRequest'];

export function requestUpgradeAlbum() {
	return createAppMutation(() => ({
		mutationFn: (input: UpgradeAlbumInput) =>
			api.v3.POST(DOWNLOAD_UPGRADE_ENDPOINTS.album(), input),
		effects: afterUpgrade
	}));
}

export type UpgradeTrackInput = components['schemas']['UpgradeTrackRequest'];

export function requestUpgradeTrack() {
	return createAppMutation(() => ({
		mutationFn: (input: UpgradeTrackInput) =>
			api.v3.POST(DOWNLOAD_UPGRADE_ENDPOINTS.track(), input),
		effects: afterUpgrade
	}));
}
