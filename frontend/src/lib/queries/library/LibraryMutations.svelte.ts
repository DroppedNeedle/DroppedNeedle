import { createMutation } from '@tanstack/svelte-query';
import { API } from '$lib/constants';
import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { LOCAL_KEYS } from '../local/LocalQueries.svelte';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import type {
	LibraryActionResponse,
	StatusMessageResponse,
	LibraryScanSchedule,
	LibrarySettings,
	LibraryTrack,
	TrackTagUpdate,
	UnmatchedBatchResolveRequest,
	UnmatchedBatchResolveResponse,
	UnmatchedResolution
} from '$lib/types';

export function startLibraryScan() {
	return createMutation(() => ({
		mutationFn: () => api.global.post<LibraryActionResponse>(API.library.scanStart(), {}),
		onSuccess: () =>
			invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.scanStatus() })
	}));
}

// Force re-scan: re-identifies every file and clears the MB cache (backend reads `?force`).
export function startForceLibraryScan() {
	return createMutation(() => ({
		mutationFn: () =>
			api.global.post<LibraryActionResponse>(`${API.library.scanStart()}?force=true`, {}),
		onSuccess: () =>
			invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.scanStatus() })
	}));
}

export function cancelLibraryScan() {
	return createMutation(() => ({
		mutationFn: () => api.global.post<LibraryActionResponse>(API.library.scanCancel(), {}),
		onSuccess: () =>
			invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.scanStatus() })
	}));
}

export function resolveUnmatchedFile() {
	return createMutation(() => ({
		mutationFn: (input: { id: number; resolution: UnmatchedResolution; mbid?: string }) =>
			api.global.post<LibraryActionResponse>(API.library.resolveUnmatched(input.id), {
				resolution: input.resolution,
				mbid: input.mbid ?? null
			}),
		onSuccess: async () => {
			await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.unmatched() });
			await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.stats() });
		}
	}));
}

export function resolveUnmatchedBatch() {
	return createMutation(() => ({
		mutationFn: (input: UnmatchedBatchResolveRequest) =>
			api.global.post<UnmatchedBatchResolveResponse>(API.library.resolveUnmatchedBatch(), input),
		onSuccess: async () => {
			await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.unmatched() });
			await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.stats() });
		}
	}));
}

export function updateTrackTags() {
	return createMutation(() => ({
		mutationFn: (input: { fileId: string; releaseGroupMbid: string; tags: TrackTagUpdate }) =>
			api.global.post<LibraryTrack>(API.library.updateTrackTags(input.fileId), input.tags),
		onSuccess: (_data, input) =>
			invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.album(input.releaseGroupMbid)
			})
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

// Force a fresh whole-folder re-identification (the correction path for an album the scan
// landed on the wrong release group). Like rescan, it's a background job, so re-invalidate
// on the same delay ladder to pick up the result.
export function reidentifyAlbum() {
	return createMutation(() => ({
		mutationFn: (mbid: string) =>
			api.global.post<LibraryActionResponse>(API.library.reidentifyAlbum(mbid), {}),
		onSuccess: (_data, mbid) => {
			const invalidate = () =>
				invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.album(mbid) });
			void invalidate();
			for (const delay of RESCAN_REFRESH_DELAYS_MS) setTimeout(() => void invalidate(), delay);
		}
	}));
}

export function saveLibrarySettings() {
	return createMutation(() => ({
		mutationFn: (settings: LibrarySettings) =>
			api.global.put<LibrarySettings>(API.library.settings(), settings),
		onSuccess: () => invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.settings() })
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

export function addLibraryPath() {
	return createMutation(() => ({
		mutationFn: (path: string) => api.global.post<LibrarySettings>(API.library.addPath(), { path }),
		onSuccess: () => invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.settings() })
	}));
}

export function removeLibraryPath() {
	return createMutation(() => ({
		mutationFn: (path: string) => api.global.delete<LibrarySettings>(API.library.removePath(path)),
		onSuccess: () => invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.settings() })
	}));
}

// Remove ONE library file - the album page's orphan-review action (P5): a held
// file that matches none of the album's expected tracks. Admin/trusted only
// (the route enforces it). Invalidates the album's coverage/status AND the
// local-library lists (cross-domain: sizes and sidebars change with the file).
export function removeLibraryTrack() {
	return createMutation(() => ({
		mutationFn: ({ fileId }: { fileId: string; albumMbid: string }) =>
			api.global.delete<StatusMessageResponse>(API.library.removeTrack(fileId)),
		onSuccess: async (_data, { albumMbid }) => {
			await invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.album(albumMbid)
			});
			await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.stats() });
			await invalidateQueriesWithPersister({ queryKey: LOCAL_KEYS.root });
		}
	}));
}
