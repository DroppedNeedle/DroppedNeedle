import { v3 } from '$lib/api/v3/endpoint';
import { useQueryClient } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { isStaleRevision } from '$lib/api/errors';
import { authStore } from '$lib/stores/authStore.svelte';
import { libraryStore } from '$lib/stores/library';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import type { LibraryAlbumStatus } from '$lib/features/library/queries/types';
import type { components } from '$lib/api/v3/openapi';
import {
	albumLocalCopyChanged,
	albumRemovalsChanged,
	albumStatusChanged,
	anyWantedChanged,
	applyEffects,
	applyEffectsIn,
	catalogChanged,
	createAppMutation,
	editionPlansChanged,
	libraryCountsChanged,
	localChanged,
	scanScheduleChanged
} from '$lib/queries/effects';

type LibraryScanSchedule = components['schemas']['LibraryScanSchedule'];

export function createRemoveLibraryAlbumMutation() {
	return createAppMutation(() => ({
		// `mbid` is the address the caller knows the album by; the rest comes
		// from the album's detail.
		mutationFn: ({
			albumId,
			expectedRevision,
			expectedTrackCount,
			stopWanted
		}: {
			mbid: string;
			albumId: string;
			expectedRevision: number;
			expectedTrackCount: number;
			stopWanted: boolean;
		}) =>
			api.v3.DELETE(
				v3('/api/v3/library/albums/{id}', {
					path: { id: albumId },
					query: {
						expected_revision: expectedRevision,
						expected_track_count: expectedTrackCount,
						delete_files: true,
						stop_wanted: stopWanted
					}
				})
			),
		onSuccess: async (result, { mbid: requestedMbid }) => {
			const removedMbids = [requestedMbid, result.id].filter(
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
									tracks: []
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

export function createRescanAlbumMutation() {
	return createAppMutation(() => ({
		mutationFn: (mbid: string) =>
			api.v3.POST(v3('/api/v3/library/albums/{album_id}/rescan', { path: { album_id: mbid } })),
		onSuccess: (_data, mbid) => {
			const status = albumStatusChanged(authStore.user?.id, mbid);
			for (const delay of RESCAN_REFRESH_DELAYS_MS) {
				setTimeout(() => void applyEffects(status), delay);
			}
		},
		effects: (_data, mbid) => [albumStatusChanged(authStore.user?.id, mbid)]
	}));
}

export function createSaveLibraryScanScheduleMutation() {
	return createAppMutation(() => ({
		mutationFn: (schedule: LibraryScanSchedule) =>
			api.v3.PUT(v3('/api/v3/settings/library/schedule'), {
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
export function createRemoveLibraryTrackMutation() {
	const client = useQueryClient();
	const changed = (albumMbid: string, albumId?: string) => {
		const userId = authStore.user?.id;
		return [
			albumStatusChanged(userId, albumMbid),
			albumLocalCopyChanged(userId, albumMbid),
			libraryCountsChanged(userId),
			albumRemovalsChanged(userId),
			localChanged(),
			...(albumId ? [editionPlansChanged(userId, albumId)] : [])
		];
	};
	return createAppMutation(
		() => ({
			// `albumId`, the library album's own id, also refreshes its edition plans.
			mutationFn: ({
				fileId,
				expectedRevision
			}: {
				fileId: string;
				expectedRevision: number;
				albumMbid: string;
				albumId?: string;
			}) =>
				api.v3.DELETE(
					v3('/api/v3/library/tracks/{id}', {
						path: { id: fileId },
						query: { expected_revision: expectedRevision }
					})
				),
			// A refusal for a moved revision means the page is old: read it again.
			onError: (error, { albumMbid, albumId }) => {
				if (!isStaleRevision(error)) return;
				void applyEffectsIn(client, changed(albumMbid, albumId)).catch((failure: unknown) =>
					console.error('Refreshing after a refused removal failed', failure)
				);
			},
			effects: (_data, { albumMbid, albumId }) => changed(albumMbid, albumId)
		}),
		client
	);
}
