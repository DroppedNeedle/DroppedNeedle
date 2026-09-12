import { createMutation } from '@tanstack/svelte-query';
import { API } from '$lib/constants';
import { api } from '$lib/api/client';
import { libraryStore } from '$lib/stores/library';
import { ArtistQueryKeyFactory } from '../artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '../discover/DiscoverQueryKeyFactory';
import { HomeQueryKeyFactory } from '../HomeQueryKeyFactory';
import { WantedQueryKeyFactory } from '../wanted/WantedQueryKeyFactory';
import { invalidateQueriesWithPersister, setQueryDataWithPersister } from '../QueryClient';
import { LOCAL_KEYS } from '../local/LocalQueries.svelte';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { albumSourceMatchCache } from '$lib/utils/albumDetailCache';
import type {
	AlbumRemoveResponse,
	TargetCatalogRemovalResponse,
	LibraryActionResponse,
	LibraryAlbumStatus,
	StatusMessageResponse,
	LibraryScanSchedule
} from '$lib/types';

export function removeLibraryAlbum() {
	return createMutation(() => ({
		mutationFn: ({ mbid, stopWanted }: { mbid: string; stopWanted: boolean }) =>
			api.global.delete<AlbumRemoveResponse | TargetCatalogRemovalResponse>(
				`${API.library.removeAlbum(mbid)}?delete_files=true&stop_wanted=${stopWanted}`
			),
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
		mutationFn: (mbid: string) =>
			api.global.post<LibraryActionResponse>(API.library.rescanAlbum(mbid), {}),
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
			api.global.put<LibraryScanSchedule>(API.library.scanSchedule(), schedule),
		onSuccess: () =>
			invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.scanSchedule() })
	}));
}

// Remove ONE library file by id: the orphan-review action (P5) and the matched
// track page's per-row action. Admin/trusted only (the route enforces it).
// Invalidates the album's coverage/status AND the local-library lists, and
// clears the caller's captured source-match cache entry (the exact key string
// taken when the action started) here rather than in the page, so a page that
// unmounts before settle cannot leave the entry behind.
export function removeLibraryTrack() {
	return createMutation(() => ({
		mutationFn: ({ fileId }: { fileId: string; albumMbid: string; albumCacheKey: string }) =>
			api.global.delete<StatusMessageResponse>(API.library.removeTrack(fileId)),
		onSuccess: async (_data, { albumMbid, albumCacheKey }) => {
			albumSourceMatchCache.remove(albumCacheKey);
			await invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.album(albumMbid)
			});
			await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.stats() });
			await invalidateQueriesWithPersister({ queryKey: LOCAL_KEYS.root });
		}
	}));
}
