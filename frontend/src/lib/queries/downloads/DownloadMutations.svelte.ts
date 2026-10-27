import { createMutation } from '@tanstack/svelte-query';
import { get } from 'svelte/store';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { REQUESTS_ENDPOINTS } from '$lib/queries/requests/endpoints';
import { RequestQueryKeyFactory } from '$lib/queries/requests/RequestQueryKeyFactory';
import type { IntakeResponse, TrackIntakeResponse } from '$lib/queries/requests/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { integrationStore } from '$lib/stores/integration';
import { libraryStore } from '$lib/stores/library';
import { toastStore } from '$lib/stores/toast';
// Request-surface copy lives beside the other acquisition label mappings.
import {
	WAITING_FOR_SOURCE_COPY,
	batchRequestCopy,
	requestStatusCopy
} from '$lib/utils/acquisitionLabels';
import { albumRequestOutcome } from '$lib/utils/requestOutcome';
import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_TASKS_ENDPOINTS } from './endpoints';

interface AlbumRequestInput {
	release_group_mbid: string;
	artist_name?: string | null;
	album_title?: string | null;
	year?: number | null;
	artist_mbid?: string | null;
	/** The edition to fetch; omitted, the library's chosen edition is fetched. */
	release_mbid?: string | null;
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

// A request accepted while no download source is set up waits for one; the
// toast says so instead of claiming a search began.
function noSourceReady(): boolean {
	const status = get(integrationStore);
	return status.loaded && !status.download_client;
}

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

// One consolidated request surface means one invalidation pair (Acquisition plan):
// an accepted/dispatched/duplicate album answer may have created tasks or history
// rows. DownloadQueryKeyFactory.all already prefixes tasks/activity/held/policy.
async function invalidateRequestSurface(): Promise<void> {
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.all }),
		invalidateQueriesWithPersister({ queryKey: RequestQueryKeyFactory.all })
	]);
}

// One click, one toast, one cache path (Acquisition "Request surfaces"). The
// response status is mapped to the spec's four copy lines by albumRequestOutcome;
// the summary sentence comes from the response when the backend carries one.
export function requestAlbum() {
	return createMutation(() => ({
		mutationFn: async (input: AlbumRequestInput): Promise<IntakeResponse> => {
			const initiatingUserId = authStore.user?.id;
			let data: IntakeResponse;
			try {
				data = await api.global.v3.POST(REQUESTS_ENDPOINTS.requestAlbum(), {
					musicbrainz_id: input.release_group_mbid,
					artist: input.artist_name ?? null,
					album: input.album_title ?? null,
					year: input.year ?? null,
					artist_mbid: input.artist_mbid ?? null,
					release_mbid: input.release_mbid ?? null,
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
			const outcome = albumRequestOutcome(data) ?? 'dispatched';
			toastStore.show({
				message:
					outcome === 'dispatched' && noSourceReady()
						? WAITING_FOR_SOURCE_COPY
						: requestStatusCopy(outcome),
				type: outcome === 'duplicate_active' || outcome === 'in_library' ? 'info' : 'success'
			});
			return data;
		}
	}));
}

export type BatchAlbumItem = components['schemas']['BatchItem'];

export interface BatchRequestResult {
	success: boolean;
	requested: number;
	skipped: number;
	overflow: number;
	error?: string;
}

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
				const response = await api.global.v3.POST(REQUESTS_ENDPOINTS.requestBatch(), {
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
			api.global.v3.POST(REQUESTS_ENDPOINTS.requestTrack(), {
				recording_mbid: input.recording_mbid,
				artist_name: input.artist_name,
				track_title: input.track_title,
				album_title: input.album_title ?? null,
				duration_seconds: input.duration_seconds ?? null,
				release_group_mbid: input.release_group_mbid ?? null,
				artist_mbid: input.artist_mbid ?? null,
				release_mbid: input.release_id ?? null
			}),
		onSuccess: (data: TrackIntakeResponse) => {
			toastStore.show({
				message:
					data.status === 'already_in_library'
						? 'That track is already in your library'
						: data.status === 'awaiting_approval'
							? 'Track request submitted for admin approval'
							: noSourceReady()
								? WAITING_FOR_SOURCE_COPY
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
		mutationFn: (id: string) => api.global.v3.POST(DOWNLOAD_TASKS_ENDPOINTS.cancel(id)),
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
			api.global.v3.POST(DOWNLOAD_TASKS_ENDPOINTS.nextSource(input.id), {
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
		mutationFn: (id: string) => api.global.v3.POST(DOWNLOAD_TASKS_ENDPOINTS.cancel(id)),
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
		mutationFn: (id: string) => api.global.v3.POST(DOWNLOAD_TASKS_ENDPOINTS.retry(id)),
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
		mutationFn: () => api.global.v3.POST(DOWNLOAD_TASKS_ENDPOINTS.clear()),
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
		mutationFn: () => api.global.v3.POST(DOWNLOAD_TASKS_ENDPOINTS.stopAllRetries()),
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
		mutationFn: () => api.global.v3.POST(DOWNLOAD_TASKS_ENDPOINTS.retryAllFailed()),
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

function invalidateAlbum(releaseGroupMbid?: string | null) {
	if (releaseGroupMbid) {
		void invalidateQueriesWithPersister({
			queryKey: LibraryQueryKeyFactory.album(releaseGroupMbid)
		});
	}
}

interface ReimportInput {
	id: string;
	release_group_mbid?: string | null;
}

// Admin reimport: the task goes back in line with its picked candidate
// kept, so the worker checks the downloads mount again without
// re-searching.
export function reimportDownload() {
	return createMutation(() => ({
		mutationFn: (input: ReimportInput) =>
			api.global.v3.POST(DOWNLOAD_TASKS_ENDPOINTS.reimport(input.id)),
		onSuccess: (data, input: ReimportInput) => {
			if (data.status === 'completed') {
				toastStore.show({ message: 'Import complete', type: 'success' });
			} else if (data.status === 'partial') {
				toastStore.show({
					message: 'Imported what was found, some files still missing',
					type: 'info'
				});
			} else {
				toastStore.show({
					message: data.reason?.text ?? "Couldn't find the files on the downloads mount yet",
					type: 'error'
				});
			}
			void invalidateTasks();
			void invalidateQueriesWithPersister({ queryKey: RequestQueryKeyFactory.all });
			invalidateAlbum(input.release_group_mbid);
		},
		onError: (err: unknown) =>
			toastStore.show({ message: errorMessage(err, 'Failed to reimport download'), type: 'error' })
	}));
}
