import { useQueryClient } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { libraryStore } from '$lib/stores/library';
import { setQueryDataWithPersister } from '../QueryClient';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import type {
	AlbumRemoveResponse,
	TargetCatalogRemovalResponse,
	LibraryAlbumStatus,
	LibraryScanSchedule
} from '$lib/types';
import {
	albumLocalCopyChanged,
	albumRemovalsChanged,
	albumStatusChanged,
	anyWantedChanged,
	applyEffects,
	catalogChanged,
	createAppMutation,
	editionPlansChanged,
	libraryCountsChanged,
	localChanged,
	scanScheduleChanged
} from '$lib/queries/effects';

export function removeLibraryAlbum() {
	return createAppMutation(() => ({
		mutationFn: ({ mbid, stopWanted }: { mbid: string; stopWanted: boolean }) =>
			api.v3.DELETE(LibraryV3Api.removeAlbum(mbid, stopWanted)) as Promise<
				AlbumRemoveResponse | TargetCatalogRemovalResponse
			>,
		onSuccess: async (result, { mbid: requestedMbid }) => {
			const responseMbids =
				'album_mbid' in result ? [result.album_mbid, ...result.removed_mbids] : [result.id];
			const removedMbids = [requestedMbid, ...responseMbids].filter(
				(mbid, index, all) => all.indexOf(mbid) === index
			);
			for (const mbid of removedMbids) {
				libraryStore.removeMbid(mbid);
			}
			try {
				await setQueryDataWithPersister<LibraryAlbumStatus>(
					LibraryQueryKeyFactory.album(authStore.user?.id, requestedMbid),
					(previous) =>
						previous
							? {
									...previous,
									in_library: false,
									track_count: 0,
									tracks: [],
									covered_tracks: 0,
									matched_file_ids: [],
									orphans: []
								}
							: previous
				);
			} catch (error) {
				console.error('Album removal cache update failed', error);
			}
		},
		effects: () => [catalogChanged(authStore.user?.id), anyWantedChanged()]
	}));
}

// Re-invalidate the album status a few times after a rescan. The rescan endpoint
// returns 202 and refreshes the rows on a background task with no completion event,
// so a single immediate invalidation would only re-read the pre-rescan rows.
const RESCAN_REFRESH_DELAYS_MS = [2500, 6000];

export function rescanAlbum() {
	return createAppMutation(() => ({
		mutationFn: (mbid: string) => api.v3.POST(LibraryV3Api.rescanAlbum(mbid)),
		onSuccess: (_data, mbid) => {
			const status = albumStatusChanged(authStore.user?.id, mbid);
			for (const delay of RESCAN_REFRESH_DELAYS_MS) {
				setTimeout(() => void applyEffects(status), delay);
			}
		},
		effects: (_data, mbid) => [albumStatusChanged(authStore.user?.id, mbid)]
	}));
}

export function saveLibraryScanSchedule() {
	return createAppMutation(() => ({
		mutationFn: (schedule: LibraryScanSchedule) =>
			api.v3.PUT(LibraryV3Api.schedule(), {
				scan_frequency: schedule.scan_frequency,
				daily_scan_time: schedule.daily_scan_time
			}),
		effects: () => [scanScheduleChanged()]
	}));
}

// Remove ONE library file by id: the orphan-review action and the matched
// track page's per-row action. Admin/trusted only (the route enforces it).
// Invalidates the album's coverage/status, its local copy on the album page
// AND the local-library lists, in the cache of the page that removed it.
export function removeLibraryTrack() {
	const client = useQueryClient();
	return createAppMutation(
		() => ({
			// `albumId`, the library album's own id, also refreshes its edition plans.
			mutationFn: ({ fileId }: { fileId: string; albumMbid: string; albumId?: string }) =>
				api.v3.DELETE(LibraryV3Api.removeTrack(fileId)),
			effects: (_data, { albumMbid, albumId }) => {
				const userId = authStore.user?.id;
				return [
					albumStatusChanged(userId, albumMbid),
					albumLocalCopyChanged(userId, albumMbid),
					libraryCountsChanged(userId),
					albumRemovalsChanged(userId),
					localChanged(),
					...(albumId ? [editionPlansChanged(userId, albumId)] : [])
				];
			}
		}),
		client
	);
}
