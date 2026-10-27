import { createMutation, useQueryClient } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { libraryStore } from '$lib/stores/library';
import { ArtistQueryKeyFactory } from '../artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '../discover/DiscoverQueryKeyFactory';
import { HomeQueryKeyFactory } from '../HomeQueryKeyFactory';
import { WantedQueryKeyFactory } from '../wanted/WantedQueryKeyFactory';
import { invalidateQueriesWithPersister, setQueryDataWithPersister } from '../QueryClient';
import { LOCAL_KEYS } from '../local/LocalV3Keys';
import { AlbumMatchKeyFactory } from '../albumMatch';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import type {
	AlbumRemoveResponse,
	TargetCatalogRemovalResponse,
	LibraryAlbumStatus,
	LibraryScanSchedule
} from '$lib/types';

export function removeLibraryAlbum() {
	return createMutation(() => ({
		mutationFn: ({ mbid, stopWanted }: { mbid: string; stopWanted: boolean }) =>
			api.global.v3.DELETE(LibraryV3Api.removeAlbum(mbid, stopWanted)) as Promise<
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
					LibraryQueryKeyFactory.album(requestedMbid),
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
			const refreshes = await Promise.allSettled([
				invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all }),
				invalidateQueriesWithPersister({ queryKey: ArtistQueryKeyFactory.prefix }),
				invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix }),
				invalidateQueriesWithPersister({ queryKey: DiscoverQueryKeyFactory.prefix }),
				invalidateQueriesWithPersister({ queryKey: WantedQueryKeyFactory.prefix }),
				invalidateQueriesWithPersister({ queryKey: LOCAL_KEYS.root })
			]);
			for (const refresh of refreshes) {
				if (refresh.status === 'rejected') {
					console.error('Album removal cache refresh failed', refresh.reason);
				}
			}
		}
	}));
}

// Re-invalidate the album status a few times after a rescan. The rescan endpoint
// returns 202 and refreshes the rows on a background task with no completion event,
// so a single immediate invalidation would only re-read the pre-rescan rows.
const RESCAN_REFRESH_DELAYS_MS = [2500, 6000];

export function rescanAlbum() {
	return createMutation(() => ({
		mutationFn: (mbid: string) => api.global.v3.POST(LibraryV3Api.rescanAlbum(mbid)),
		onSuccess: (_data, mbid) => {
			const invalidate = () =>
				invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.album(mbid) });
			void invalidate();
			for (const delay of RESCAN_REFRESH_DELAYS_MS) setTimeout(() => void invalidate(), delay);
		}
	}));
}

export function saveLibraryScanSchedule() {
	return createMutation(() => ({
		mutationFn: (schedule: LibraryScanSchedule) =>
			api.global.v3.PUT(LibraryV3Api.schedule(), {
				scan_frequency: schedule.scan_frequency,
				daily_scan_time: schedule.daily_scan_time
			}),
		onSuccess: () =>
			invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.scanSchedule() })
	}));
}

// Remove ONE library file by id: the orphan-review action and the matched
// track page's per-row action. Admin/trusted only (the route enforces it).
// Invalidates the album's coverage/status, its local copy on the album page
// AND the local-library lists, in the cache of the page that removed it.
export function removeLibraryTrack() {
	const client = useQueryClient();
	return createMutation(() => ({
		mutationFn: ({ fileId }: { fileId: string; albumMbid: string }) =>
			api.global.v3.DELETE(LibraryV3Api.removeTrack(fileId)),
		onSuccess: async (_data, { albumMbid }) => {
			const userId = authStore.user?.id;
			for (const queryKey of [
				LibraryQueryKeyFactory.album(albumMbid),
				AlbumMatchKeyFactory.local(userId, albumMbid),
				LibraryQueryKeyFactory.catalog.stats(userId),
				LOCAL_KEYS.root,
				LibraryQueryKeyFactory.catalog.albumRemovalsPrefix(userId)
			]) {
				await invalidateQueriesWithPersister({ queryKey }, undefined, { client });
			}
		}
	}));
}
