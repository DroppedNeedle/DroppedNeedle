import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';
import { libraryStore } from '$lib/stores/library';
import { toastStore } from '$lib/stores/toast';
// Request-surface copy lives beside the other acquisition label mappings.
import { batchRequestCopy, requestStatusCopy } from '$lib/utils/acquisitionLabels';
import { albumRequestOutcome } from '$lib/utils/requestOutcome';
import type { RequestAccepted } from '$lib/types';
import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';

// Response mirrors for the consolidated request paths that are not shared with
// the application-wide type contract.
interface CancelDownloadResponse {
	status?: string;
}

interface NextSourceResponse {
	started?: boolean;
}

interface ReimportDownloadResponse {
	status: string;
	error_message?: string | null;
}

interface RetryDownloadResponse {
	started?: boolean;
}

interface TrackRequestResponse {
	status: string;
	task_id?: string | null;
}

interface AlbumRequestInput {
	release_group_mbid: string;
	artist_name?: string | null;
	album_title?: string | null;
	year?: number | null;
	artist_mbid?: string | null;
	monitor_artist?: boolean;
	auto_download_artist?: boolean;
}

interface TrackRequestInput {
	recording_mbid: string;
	artist_name: string;
	track_title: string;
	album_title?: string | null;
	duration_seconds?: number | null;
	release_group_mbid?: string | null;
	artist_mbid?: string | null;
	release_id?: string | null;
}

const invalidateTasks = () =>
	invalidateQueriesWithPersister({
		queryKey: DownloadQueryKeyFactory.tasks(authStore.user?.id)
	});

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

// One consolidated request surface means one invalidation pair (Acquisition plan):
// an accepted/dispatched/duplicate album answer may have created tasks or history
// rows. DownloadQueryKeyFactory.all already prefixes tasks/activity/held/policy;
// ['requests'] has no factory yet (the requests page still fetches imperatively),
// so the literal prefix carries the future convention.
async function invalidateRequestSurface(): Promise<void> {
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.all }),
		invalidateQueriesWithPersister({ queryKey: ['requests'] })
	]);
}

// One click, one toast, one cache path (Acquisition "Request surfaces"). The
// response status is mapped to the spec's four copy lines by albumRequestOutcome;
// the summary sentence comes from the response when the backend carries one.
export function requestAlbum() {
	return createMutation(() => ({
		mutationFn: async (input: AlbumRequestInput): Promise<RequestAccepted> => {
			const initiatingUserId = authStore.user?.id;
			let data: RequestAccepted;
			try {
				data = await api.global.post<RequestAccepted>(API.requests.new(), {
					musicbrainz_id: input.release_group_mbid,
					artist: input.artist_name ?? null,
					album: input.album_title ?? null,
					year: input.year ?? null,
					artist_mbid: input.artist_mbid ?? null,
					...(input.monitor_artist || input.auto_download_artist
						? {
								monitor_artist: input.monitor_artist === true,
								auto_download_artist: input.auto_download_artist === true
							}
						: {})
				});
			} catch (err) {
				toastStore.show({ message: errorMessage(err, 'Request failed'), type: 'error' });
				throw err;
			}
			// Session switched while the POST was in flight: touch nothing cached and
			// report a plain failure so no badge or toast fires for the previous user.
			if (!initiatingUserId || authStore.user?.id !== initiatingUserId) {
				return { ...data, success: false };
			}

			if (data.success) {
				libraryStore.addRequested(input.release_group_mbid);
			}
			void invalidateRequestSurface();

			if (!data.success) {
				toastStore.show({ message: data.message || 'Request failed', type: 'error' });
				return data;
			}
			if (data.status === 'cancelling') {
				toastStore.show({
					message: data.message || 'Request is being cancelled',
					type: 'info'
				});
				return data;
			}
			const outcome = albumRequestOutcome(data);
			const summary = data.quality_snapshot_summary ?? undefined;
			toastStore.show({
				message: requestStatusCopy(outcome ?? 'dispatched', summary),
				type: outcome === 'duplicate_active' || outcome === 'in_library' ? 'info' : 'success'
			});
			return data;
		}
	}));
}

export interface BatchAlbumItem {
	musicbrainz_id: string;
	artist_name?: string;
	album_title?: string;
	year?: number | null;
	artist_mbid?: string;
}

export interface BatchRequestResult {
	success: boolean;
	requested: number;
	skipped: number;
	overflow: number;
	error?: string;
}

// constants.ts carries registry rows only; the batch endpoint predates this
// slice's read-only rule for that file, so its URL stays local to this module.
const REQUEST_BATCH_URL = '/api/v1/requests/batch';

// Discography/discovery bulk requests. Counts are rendered verbatim by
// batchRequestCopy - skipped and over-limit albums are never claimed as queued.
export function requestBatch() {
	return createMutation(() => ({
		mutationFn: async (input: {
			items: BatchAlbumItem[];
			monitorArtist?: boolean;
			autoDownloadArtist?: boolean;
		}): Promise<BatchRequestResult> => {
			const initiatingUserId = authStore.user?.id;
			try {
				const response = await api.global.post<{
					success: boolean;
					message: string;
					requested: number;
					skipped: number;
					overflow: number;
				}>(REQUEST_BATCH_URL, {
					items: input.items,
					monitor_artist: input.monitorArtist === true,
					auto_download_artist: input.autoDownloadArtist === true
				});
				if (!initiatingUserId || authStore.user?.id !== initiatingUserId) {
					return {
						success: false,
						requested: response.requested,
						skipped: response.skipped,
						overflow: response.overflow
					};
				}
				if (response.success) {
					for (const item of input.items) {
						libraryStore.addRequested(item.musicbrainz_id);
					}
				}
				void invalidateRequestSurface();
				toastStore.show({
					message: batchRequestCopy(response.requested, response.skipped, response.overflow),
					type: 'info'
				});
				return {
					success: response.success,
					requested: response.requested,
					skipped: response.skipped,
					overflow: response.overflow
				};
			} catch (err) {
				const detail = err instanceof Error && err.message ? err.message : 'Network error occurred';
				toastStore.show({ message: detail, type: 'error' });
				return { success: false, requested: 0, skipped: 0, overflow: 0, error: detail };
			}
		}
	}));
}

export function requestTrack() {
	return createMutation(() => ({
		mutationFn: (input: TrackRequestInput) =>
			api.global.post<TrackRequestResponse>(API.tracks.request(input.recording_mbid), {
				artist_name: input.artist_name,
				track_title: input.track_title,
				album_title: input.album_title ?? null,
				duration_seconds: input.duration_seconds ?? null,
				release_group_mbid: input.release_group_mbid ?? null,
				artist_mbid: input.artist_mbid ?? null,
				release_id: input.release_id ?? null
			}),
		onSuccess: (data: TrackRequestResponse) => {
			toastStore.show({
				message:
					data.status === 'already_in_library'
						? 'That track is already in your library'
						: data.status === 'awaiting_approval'
							? 'Track request submitted for admin approval'
							: 'Track requested - searching for downloads',
				type: 'success'
			});
			void invalidateTasks();
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Track request failed'), type: 'error' })
	}));
}

export function cancelDownload() {
	return createMutation(() => ({
		mutationFn: (id: string) =>
			api.global.post<CancelDownloadResponse>(API.downloads.cancel(id), {}),
		onSuccess: () => {
			toastStore.show({ message: 'Download cancelled', type: 'info' });
			void invalidateTasks();
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to cancel download'), type: 'error' })
	}));
}

interface NextSourceInput {
	id: string;
	candidateIndex: number;
}

export function tryNextSource() {
	return createMutation(() => ({
		mutationFn: (input: NextSourceInput) =>
			api.global.post<NextSourceResponse>(API.downloads.nextSource(input.id), {
				expected_candidate_index: input.candidateIndex
			}),
		onSuccess: () => {
			toastStore.show({ message: 'Trying the next source', type: 'info' });
			void invalidateTasks();
		},
		onError: (err: unknown) => {
			void invalidateTasks();
			toastStore.show({
				message: errorMessage(err, 'Could not switch sources'),
				type: 'error'
			});
		}
	}));
}

// Stop a scheduled auto-retry. Cancelling the failed/partial task drops it out of the
// retry sweep (status -> cancelled); a manual Retry is still available afterwards.
export function stopAutoRetry() {
	return createMutation(() => ({
		mutationFn: (id: string) =>
			api.global.post<CancelDownloadResponse>(API.downloads.cancel(id), {}),
		onSuccess: () => {
			toastStore.show({ message: 'Stopped retrying', type: 'info' });
			void invalidateTasks();
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to stop retrying'), type: 'error' })
	}));
}

export function retryDownload() {
	return createMutation(() => ({
		mutationFn: (id: string) => api.global.post<RetryDownloadResponse>(API.downloads.retry(id), {}),
		onSuccess: () => {
			toastStore.show({ message: 'Download retry initiated', type: 'info' });
			void invalidateTasks();
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to retry download'), type: 'error' })
	}));
}

// Bulk queue actions for the dashboard. Each reports how many rows it touched so the
// toast is honest ("Cleared 12 downloads", "Nothing to clear").
function pluralDownloads(n: number): string {
	return `${n} download${n === 1 ? '' : 's'}`;
}

export function clearFinished() {
	return createMutation(() => ({
		mutationFn: () => api.global.post<{ cleared: number }>(API.downloads.clear(), {}),
		onSuccess: (data: { cleared: number }) => {
			toastStore.show({
				message: data.cleared > 0 ? `Cleared ${pluralDownloads(data.cleared)}` : 'Nothing to clear',
				type: 'info'
			});
			void invalidateTasks();
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to clear downloads'), type: 'error' })
	}));
}

export function stopAllRetries() {
	return createMutation(() => ({
		mutationFn: () => api.global.post<{ stopped: number }>(API.downloads.stopAllRetries(), {}),
		onSuccess: (data: { stopped: number }) => {
			toastStore.show({
				message:
					data.stopped > 0
						? `Stopped retrying ${pluralDownloads(data.stopped)}`
						: 'No retries to stop',
				type: 'info'
			});
			void invalidateTasks();
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to stop retries'), type: 'error' })
	}));
}

export function retryAllFailed() {
	return createMutation(() => ({
		mutationFn: () => api.global.post<{ retried: number }>(API.downloads.retryAllFailed(), {}),
		onSuccess: (data: { retried: number }) => {
			toastStore.show({
				message:
					data.retried > 0 ? `Retrying ${pluralDownloads(data.retried)}` : 'Nothing to retry',
				type: 'info'
			});
			void invalidateTasks();
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to retry downloads'), type: 'error' })
	}));
}

// Held-track review actions. Both refresh the held list + the queue (invalidateTasks covers
// the held query, nested under the tasks key); import also refreshes the album's library
// status so the newly-placed track shows on the album page.
interface HeldActionInput {
	id: number;
	release_group_mbid?: string | null;
}

function invalidateAlbum(releaseGroupMbid?: string | null) {
	if (releaseGroupMbid) {
		void invalidateQueriesWithPersister({
			queryKey: LibraryQueryKeyFactory.album(releaseGroupMbid)
		});
	}
}

export function importHeldTrack() {
	return createMutation(() => ({
		mutationFn: (input: HeldActionInput) =>
			api.global.post<{ status: string; final_path: string | null }>(
				API.downloads.heldImport(input.id),
				{}
			),
		onSuccess: (_data: { status: string }, input: HeldActionInput) => {
			toastStore.show({ message: 'Imported', type: 'success' });
			void invalidateTasks();
			invalidateAlbum(input.release_group_mbid);
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to import track'), type: 'error' })
	}));
}

export function discardHeldTrack() {
	return createMutation(() => ({
		mutationFn: (input: HeldActionInput) =>
			api.global.post<{ status: string }>(API.downloads.heldDiscard(input.id), {}),
		onSuccess: () => {
			toastStore.show({ message: 'Discarded', type: 'info' });
			void invalidateTasks();
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to discard track'), type: 'error' })
	}));
}

// Re-run the fingerprint identity check on one held file. A confident result that no
// longer disagrees imports through the same settle path as "import anyway"; anything
// else stays held. invalidateTasks covers the held prefix nested under tasks; only an
// import places a track, so the album key refreshes solely on 'imported'.
export interface HeldReverifyResponse {
	status: 'imported' | 'still_held';
	final_path: string | null;
}

export function reverifyHeldTrack() {
	return createMutation(() => ({
		mutationFn: (input: HeldActionInput) =>
			api.global.post<HeldReverifyResponse>(API.downloads.heldReverify(input.id), {}),
		onSuccess: (data: HeldReverifyResponse, input: HeldActionInput) => {
			if (data.status === 'imported') {
				toastStore.show({ message: 'Re-check confirmed it: imported', type: 'success' });
				invalidateAlbum(input.release_group_mbid);
			} else {
				toastStore.show({ message: 'Still held after re-check', type: 'info' });
			}
			void invalidateTasks();
			void invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.heldPrefix(authStore.user?.id)
			});
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to re-check track'), type: 'error' })
	}));
}

export interface HeldBulkReverifyItem {
	held_id: number;
	status: 'imported' | 'still_held' | 'skipped' | 'error';
	final_path: string | null;
	release_group_mbid: string | null;
	message: string | null;
}

export interface HeldBulkReverifyResponse {
	results: HeldBulkReverifyItem[];
}

export interface HeldBulkReverifyInput {
	held_ids?: number[] | null;
}

// Re-check held tracks in bulk. held_ids scopes the run (omitted sweeps everything the
// caller may see); the 25-id cap is enforced server-side, so ids pass through untouched.
export function reverifyHeldBulk() {
	return createMutation(() => ({
		mutationFn: (input: HeldBulkReverifyInput) =>
			api.global.post<HeldBulkReverifyResponse>(API.downloads.heldReverifyBulk(), {
				held_ids: input.held_ids ?? null
			}),
		onSuccess: (data: HeldBulkReverifyResponse) => {
			const imported = data.results.filter((r) => r.status === 'imported');
			const stillHeld = data.results.filter((r) => r.status === 'still_held').length;
			const skipped = data.results.filter((r) => r.status === 'skipped').length;
			const failed = data.results.length - imported.length - stillHeld - skipped;
			if (data.results.length === 0) {
				toastStore.show({ message: 'Nothing to re-check', type: 'info' });
			} else {
				const parts = [
					imported.length > 0 ? `${imported.length} imported` : null,
					stillHeld > 0 ? `${stillHeld} still held` : null,
					skipped > 0 ? `${skipped} skipped` : null,
					failed > 0 ? `${failed} failed` : null
				].filter((p): p is string => p !== null);
				toastStore.show({
					message: `Re-checked ${data.results.length}: ${parts.join(', ')}`,
					type: imported.length > 0 ? 'success' : 'info'
				});
			}
			void invalidateTasks();
			void invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.heldPrefix(authStore.user?.id)
			});
			for (const item of imported) {
				invalidateAlbum(item.release_group_mbid);
			}
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to re-check tracks'), type: 'error' })
	}));
}

interface HeldManagementActionInput {
	taskId: string;
	releaseGroupMbid?: string | null;
}

export function retryHeldManagementUnit() {
	return createMutation(() => ({
		mutationFn: (input: HeldManagementActionInput) =>
			api.global.post<{ status: string; files: number }>(
				API.downloads.heldManagementRetry(input.taskId),
				{}
			),
		onSuccess: (data: { files: number }, input: HeldManagementActionInput) => {
			toastStore.show({
				message: `${data.files} secured ${data.files === 1 ? 'file' : 'files'} organized and imported`,
				type: 'success'
			});
			void invalidateTasks();
			// One organized import moves sidebar counts + recency lists; the touched
			// album gets its own key below. Other library lists (paginated grids the
			// retry cannot name) self-heal within the 1-minute global staleTime +
			// refetch-on-mount window instead of wiping `library` ALL.
			void invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.stats() });
			void invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.recentlyAdded() });
			invalidateAlbum(input.releaseGroupMbid);
		},
		onError: (err: unknown) => {
			void invalidateTasks();
			toastStore.show({
				message: errorMessage(err, 'File organization still needs attention'),
				type: 'error'
			});
		}
	}));
}

export function discardHeldManagementUnit() {
	return createMutation(() => ({
		mutationFn: (input: HeldManagementActionInput) =>
			api.global.post<{ status: string; files: number }>(
				API.downloads.heldManagementDiscard(input.taskId),
				{}
			),
		onSuccess: (data: { files: number }) => {
			toastStore.show({
				message: `${data.files} secured ${data.files === 1 ? 'file' : 'files'} discarded`,
				type: 'info'
			});
			void invalidateTasks();
		},
		onError: (err: unknown) => {
			void invalidateTasks();
			toastStore.show({
				message: errorMessage(err, 'Failed to discard secured files'),
				type: 'error'
			});
		}
	}));
}

interface ReimportInput {
	id: string;
	release_group_mbid?: string | null;
}

export function reimportDownload() {
	return createMutation(() => ({
		mutationFn: (input: ReimportInput) =>
			api.global.post<ReimportDownloadResponse>(API.downloads.reimport(input.id), {}),
		onSuccess: (data: ReimportDownloadResponse, input: ReimportInput) => {
			if (data.status === 'completed') {
				toastStore.show({ message: 'Import complete', type: 'success' });
			} else if (data.status === 'partial') {
				toastStore.show({
					message: 'Imported what was found, some files still missing',
					type: 'info'
				});
			} else {
				toastStore.show({
					message: data.error_message ?? "Couldn't find the files on the downloads mount yet",
					type: 'error'
				});
			}
			void invalidateTasks();
			// A completed/partial reimport writes files into the library; refresh the album
			// so its page/badge don't show stale data (the persister survives reloads).
			if (data.status === 'completed' || data.status === 'partial') {
				invalidateAlbum(input.release_group_mbid);
			}
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to reimport download'), type: 'error' })
	}));
}
