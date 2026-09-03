import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DownloadTask } from '$lib/types';

const h = vi.hoisted(() => ({
	items: [] as DownloadTask[],
	quarantine: [] as unknown[],
	held: [] as unknown[],
	isAdmin: false,
	reverifySingleMut: vi.fn(),
	reverifyBulkMut: vi.fn()
}));

vi.mock('$lib/queries/downloads/HeldQueries.svelte', () => ({
	getHeldImportsQuery: () => ({
		get data() {
			return { items: h.held };
		},
		isLoading: false,
		isError: false
	})
}));

vi.mock('$lib/queries/downloads/DownloadQueries.svelte', () => ({
	getDownloadsQuery: () => ({
		get data() {
			return { items: h.items, page: 1, page_size: 100 };
		},
		isLoading: false,
		isError: false
	})
}));

vi.mock('$lib/queries/downloads/QuarantineQueries.svelte', () => ({
	getQuarantineQuery: () => ({
		get data() {
			return { items: h.quarantine, page: 1 };
		},
		isLoading: false,
		isError: false
	}),
	deleteQuarantineEntry: () => ({ mutate: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	cancelDownload: () => ({ mutate: vi.fn(), isPending: false }),
	retryDownload: () => ({ mutate: vi.fn(), isPending: false }),
	stopAutoRetry: () => ({ mutate: vi.fn(), isPending: false }),
	clearFinished: () => ({ mutate: vi.fn(), isPending: false }),
	stopAllRetries: () => ({ mutate: vi.fn(), isPending: false }),
	retryAllFailed: () => ({ mutate: vi.fn(), isPending: false }),
	importHeldTrack: () => ({ mutate: vi.fn(), isPending: false }),
	discardHeldTrack: () => ({ mutate: vi.fn(), isPending: false }),
	reverifyHeldTrack: () => ({ mutate: h.reverifySingleMut, isPending: false }),
	reverifyHeldBulk: () => ({ mutate: h.reverifyBulkMut, isPending: false }),
	retryHeldManagementUnit: () => ({ mutate: vi.fn(), isPending: false }),
	discardHeldManagementUnit: () => ({ mutate: vi.fn(), isPending: false }),
	reimportDownload: () => ({ mutate: vi.fn(), isPending: false }),
	tryNextSource: () => ({ mutate: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/downloads/DownloadSSE.svelte', () => ({
	createDownloadStream: () => ({
		state: { progress: null, status: null, source: null, done: false },
		start: vi.fn(),
		stop: vi.fn()
	})
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: {
		get isAdmin() {
			return h.isAdmin;
		},
		get user() {
			return { id: 'u' };
		}
	}
}));

import DownloadQueue from './DownloadQueue.svelte';

function task(overrides: Partial<DownloadTask> = {}): DownloadTask {
	return {
		id: 't',
		user_id: 'u',
		download_type: 'album',
		source: 'soulseek',
		release_group_mbid: 'rg',
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: null,
		artist_name: 'Radiohead',
		album_title: 'OK Computer',
		track_title: null,
		year: 1997,
		status: 'downloading',
		progress_percent: 40,
		total_size_bytes: 1000,
		downloaded_bytes: 400,
		files_total: 12,
		files_completed: 5,
		files_failed: 0,
		source_username: 'peer',
		search_job_id: 'j',
		candidate_index: 0,
		preflight_score: 0.8,
		final_path: null,
		error_message: null,
		retry_count: 0,
		created_at: 0,
		updated_at: 0,
		completed_at: null,
		next_retry_at: null,
		retry_max: 6,
		retry_ladder_minutes: [15, 30, 60, 120, 240, 480],
		acquisition_cleanup_state: 'not_tracked',
		quality_format: null,
		quality_bit_depth: null,
		quality_sample_rate: null,
		advertised_queue_depth: null,
		queue_position_start: null,
		queue_position_end: null,
		remote_queued: false,
		preferred_quality_fallback_at: null,
		attempt_number: 0,
		attempt_total: 0,
		has_next_source: false,
		held_for_review: false,
		...overrides
	};
}

// a failed album waiting on its next scheduled auto-retry
function wanted(overrides: Partial<DownloadTask> = {}): DownloadTask {
	return task({
		id: 'w',
		album_title: 'Kid A',
		status: 'failed',
		retry_count: 1,
		next_retry_at: Date.now() / 1000 + 600,
		completed_at: Date.now() / 1000 - 120,
		...overrides
	});
}

describe('DownloadQueue.svelte', () => {
	beforeEach(() => {
		h.items = [];
		h.quarantine = [];
		h.held = [];
		h.isAdmin = false;
		h.reverifySingleMut.mockReset();
		h.reverifyBulkMut.mockReset();
	});

	it('shows the empty turntable state when there are no downloads', async () => {
		render(DownloadQueue);
		await expect.element(page.getByText('Nothing on the turntable')).toBeVisible();
	});

	it('groups an active download under Now spinning', async () => {
		h.items = [task({ id: 'a', album_title: 'In Rainbows', status: 'downloading' })];
		render(DownloadQueue);
		await expect.element(page.getByRole('heading', { name: /Now spinning/ })).toBeVisible();
		await expect.element(page.getByText('In Rainbows').first()).toBeVisible();
	});

	it('shows queued Soulseek quality and source details on the Downloads page', async () => {
		h.items = [
			task({
				downloaded_bytes: 0,
				progress_percent: 0,
				quality_format: 'flac',
				quality_bit_depth: 24,
				quality_sample_rate: 48_000,
				advertised_queue_depth: 2710,
				remote_queued: true,
				attempt_number: 1,
				attempt_total: 3,
				has_next_source: true
			})
		];
		render(DownloadQueue);

		await expect.element(page.getByText('24-bit / 48 kHz FLAC').first()).toBeVisible();
		await expect
			.element(page.getByText('Waiting for Soulseek · queue 2,710').first())
			.toBeVisible();
		await expect.element(page.getByText('Trying source 1 of 3').first()).toBeVisible();
	});

	it('shows a scheduled-retry album in the Still hunting section with its ladder and countdown', async () => {
		h.items = [wanted()];
		render(DownloadQueue);
		await expect.element(page.getByRole('heading', { name: /Still hunting/ })).toBeVisible();
		await expect.element(page.getByText('Kid A').first()).toBeVisible();
		await expect.element(page.getByText(/retry 2 of 6/)).toBeVisible();
		// the next-attempt countdown ("10:00") and a ladder rung
		await expect.element(page.getByText('10:00')).toBeVisible();
	});

	it('summarises the queue in the system pulse', async () => {
		h.items = [task({ id: 'a', status: 'downloading' }), wanted()];
		render(DownloadQueue);
		await expect.element(page.getByText('spinning').first()).toBeVisible();
		await expect.element(page.getByText('still hunting').first()).toBeVisible();
	});

	it('collapses terminal downloads into an expandable History section', async () => {
		h.items = [task({ id: 'c', album_title: 'Amnesiac', status: 'completed' })];
		render(DownloadQueue);
		const header = page.getByRole('button', { name: /History/ });
		await expect.element(header).toBeVisible();
		await expect.element(header).toHaveTextContent(/1 in your crate/);
		// collapsed by default; expand to reveal the row + the Clear control
		await header.click();
		await expect.element(page.getByText('Amnesiac').first()).toBeVisible();
		await expect.element(page.getByRole('button', { name: /Clear/ })).toBeVisible();
	});

	it('hides Quarantine from non-admins and shows it (with entries) to admins', async () => {
		h.quarantine = [
			{ id: 1, filename: '/x.flac', username: 'p', reason: 'dupe', quarantined_at: 0 }
		];
		render(DownloadQueue);
		await expect.element(page.getByRole('button', { name: /Quarantine/ })).not.toBeInTheDocument();

		h.isAdmin = true;
		render(DownloadQueue);
		await expect.element(page.getByRole('button', { name: /Quarantine/ })).toBeVisible();
	});

	it('surfaces held tracks in a "Couldn\'t verify" section', async () => {
		h.items = [task({ id: 't', status: 'failed', held_for_review: true })];
		h.held = [
			{
				id: 1,
				release_group_mbid: 'rg-1',
				release_mbid: null,
				release_track_mbid: null,
				recording_mbid: 'rec-3',
				track_number: 3,
				disc_number: 1,
				track_title: 'You Shook Me',
				artist_name: 'Led Zeppelin',
				album_title: 'Led Zeppelin',
				year: 1969,
				original_filename: 'x.flac',
				file_format: 'flac',
				duration_seconds: 388,
				expected_duration_seconds: 390,
				reason: 'fingerprint_mismatch',
				reason_detail: null,
				source: 'usenet',
				source_task_id: 't',
				created_at: 0,
				evidence_title: "Nobody's Fault but Mine",
				evidence_artist: 'Led Zeppelin',
				evidence_score: 0.99,
				management_retry_count: 0,
				management_next_retry_at: null
			}
		];
		render(DownloadQueue);
		await expect.element(page.getByRole('heading', { name: /Couldn't verify/ })).toBeVisible();
		await expect.element(page.getByText(/You Shook Me/).first()).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Retry download' }))
			.not.toBeInTheDocument();
	});

	it('groups a Library Management hold as one secured album instead of verification failures', async () => {
		h.isAdmin = true;
		h.items = [
			task({
				id: 'managed-task',
				status: 'failed',
				held_for_review: true,
				error_message: 'Download complete. The files are secured.'
			})
		];
		h.held = [1, 2].map((track) => ({
			id: track,
			release_group_mbid: 'rg',
			release_mbid: null,
			release_track_mbid: null,
			recording_mbid: `rec-${track}`,
			track_number: track,
			disc_number: 1,
			track_title: `Track ${track}`,
			artist_name: 'Radiohead',
			album_title: 'OK Computer',
			year: 1997,
			original_filename: `${track}.flac`,
			file_format: 'flac',
			duration_seconds: 200,
			expected_duration_seconds: null,
			reason: 'management:PROFILE_CHANGED',
			reason_detail: 'The active profile changed during preparation.',
			source: 'soulseek',
			source_task_id: 'managed-task',
			created_at: track,
			evidence_title: null,
			evidence_artist: null,
			evidence_score: null,
			management_retry_count: 0,
			management_next_retry_at: null
		}));

		render(DownloadQueue);

		await expect
			.element(page.getByRole('heading', { name: /Organizer needs attention/ }))
			.toBeVisible();
		await expect.element(page.getByText('2 files safely held')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Retry organizer' })).toBeVisible();
		await expect
			.element(page.getByRole('heading', { name: /Couldn't verify/ }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: 'Retry download' }))
			.not.toBeInTheDocument();
	});

	it('re-checks every visible verification hold on "Re-check all"', async () => {
		h.held = [
			{
				id: 1,
				release_group_mbid: 'rg-1',
				release_mbid: null,
				release_track_mbid: null,
				recording_mbid: 'rec-3',
				track_number: 3,
				disc_number: 1,
				track_title: 'You Shook Me',
				artist_name: 'Led Zeppelin',
				album_title: 'Led Zeppelin',
				year: 1969,
				original_filename: 'x.flac',
				file_format: 'flac',
				duration_seconds: 388,
				expected_duration_seconds: 390,
				reason: 'fingerprint_mismatch',
				reason_detail: null,
				source: 'usenet',
				source_task_id: 't',
				created_at: 0,
				evidence_title: "Nobody's Fault but Mine",
				evidence_artist: 'Led Zeppelin',
				evidence_score: 0.99,
				management_retry_count: 0,
				management_next_retry_at: null
			},
			{
				id: 2,
				release_group_mbid: 'rg',
				release_mbid: null,
				release_track_mbid: null,
				recording_mbid: 'rec-2',
				track_number: 2,
				disc_number: 1,
				track_title: 'Track 2',
				artist_name: 'Radiohead',
				album_title: 'OK Computer',
				year: 1997,
				original_filename: '2.flac',
				file_format: 'flac',
				duration_seconds: 200,
				expected_duration_seconds: null,
				reason: 'management:PROFILE_CHANGED',
				reason_detail: 'The active profile changed during preparation.',
				source: 'soulseek',
				source_task_id: 'managed-task',
				created_at: 2,
				evidence_title: null,
				evidence_artist: null,
				evidence_score: null,
				management_retry_count: 0,
				management_next_retry_at: null
			}
		];
		render(DownloadQueue);
		await page.getByRole('button', { name: 'Re-check all' }).click();
		// only the verification hold is swept; the organizer hold keeps its own retry path
		expect(h.reverifyBulkMut).toHaveBeenCalledWith({ held_ids: [1] });
		expect(h.reverifySingleMut).not.toHaveBeenCalled();
	});

	it('re-checks one hold from its card without sweeping the queue', async () => {
		h.held = [
			{
				id: 1,
				release_group_mbid: 'rg-1',
				release_mbid: null,
				release_track_mbid: null,
				recording_mbid: 'rec-3',
				track_number: 3,
				disc_number: 1,
				track_title: 'You Shook Me',
				artist_name: 'Led Zeppelin',
				album_title: 'Led Zeppelin',
				year: 1969,
				original_filename: 'x.flac',
				file_format: 'flac',
				duration_seconds: 388,
				expected_duration_seconds: 390,
				reason: 'fingerprint_mismatch',
				reason_detail: null,
				source: 'usenet',
				source_task_id: 't',
				created_at: 0,
				evidence_title: "Nobody's Fault but Mine",
				evidence_artist: 'Led Zeppelin',
				evidence_score: 0.99,
				management_retry_count: 0,
				management_next_retry_at: null
			}
		];
		render(DownloadQueue);
		await page.getByRole('button', { name: 'Re-check', exact: true }).click();
		expect(h.reverifySingleMut).toHaveBeenCalledWith(
			{ id: 1, release_group_mbid: 'rg-1' },
			expect.objectContaining({ onSuccess: expect.any(Function) })
		);
		expect(h.reverifyBulkMut).not.toHaveBeenCalled();
	});
});
