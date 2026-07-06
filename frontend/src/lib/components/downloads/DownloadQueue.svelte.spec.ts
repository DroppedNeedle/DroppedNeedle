import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DownloadTask } from '$lib/types';

const h = vi.hoisted(() => ({
	items: [] as DownloadTask[],
	quarantine: [] as unknown[],
	held: [] as unknown[],
	isAdmin: false
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
	reimportDownload: () => ({ mutate: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/downloads/DownloadSSE.svelte', () => ({
	createDownloadStream: () => ({
		state: { progress: null, status: null, done: false },
		start: vi.fn(),
		stop: vi.fn()
	})
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
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
		release_group_mbid: 'rg',
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
		h.held = [
			{
				id: 1,
				release_group_mbid: 'rg-1',
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
				reason: 'fingerprint_mismatch',
				source: 'usenet',
				source_task_id: 't',
				created_at: 0,
				evidence_title: "Nobody's Fault but Mine",
				evidence_artist: 'Led Zeppelin',
				evidence_score: 0.99
			}
		];
		render(DownloadQueue);
		await expect.element(page.getByRole('heading', { name: /Couldn't verify/ })).toBeVisible();
		await expect.element(page.getByText(/You Shook Me/).first()).toBeVisible();
	});
});
