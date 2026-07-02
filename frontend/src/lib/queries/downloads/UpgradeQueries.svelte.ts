import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import type { CutoffUnmetResponse, UpgradeRequestResponse } from '$lib/types';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';

// Quality upgrades (CollectionManagement Feature B). Admin/trusted-only surfaces:
// the Requests "Upgrades" worklist + the album page's per-track/album affordances.

export const getCutoffUnmetQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled(),
		queryKey: DownloadQueryKeyFactory.cutoffUnmet(),
		queryFn: ({ signal }) =>
			api.global.get<CutoffUnmetResponse>(API.downloads.cutoffUnmet(), { signal })
	}));

async function invalidateAfterUpgrade() {
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.cutoffUnmet() });
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.tasks() });
}

export interface UpgradeAlbumInput {
	release_group_mbid: string;
	artist_name: string;
	album_title: string;
	year?: number | null;
	artist_mbid?: string | null;
}

export function requestUpgradeAlbum() {
	return createMutation(() => ({
		mutationFn: (input: UpgradeAlbumInput) =>
			api.global.post<UpgradeRequestResponse>(API.downloads.upgradeAlbum(), input),
		onSuccess: invalidateAfterUpgrade
	}));
}

export interface UpgradeTrackInput {
	recording_mbid: string;
	artist_name: string;
	track_title: string;
	album_title?: string | null;
	duration_seconds?: number | null;
	release_group_mbid?: string | null;
	artist_mbid?: string | null;
}

export function requestUpgradeTrack() {
	return createMutation(() => ({
		mutationFn: (input: UpgradeTrackInput) =>
			api.global.post<UpgradeRequestResponse>(API.downloads.upgradeTrack(), input),
		onSuccess: invalidateAfterUpgrade
	}));
}
