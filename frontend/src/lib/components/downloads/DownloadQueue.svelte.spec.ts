import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DownloadTask } from '$lib/types';

const h = vi.hoisted(() => ({
	items: [] as DownloadTask[],
	quarantine: [] as unknown[],
	isAdmin: false
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
	retryDownload: () => ({ mutate: vi.fn(), isPending: false })
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
		next_retry_at: null,
		retry_max: 6,
		...overrides
	};
}

describe('DownloadQueue.svelte', () => {
	beforeEach(() => {
		h.items = [];
		h.quarantine = [];
		h.isAdmin = false;
	});

	it('shows the active empty state when there are no downloads', async () => {
		render(DownloadQueue);
		await expect.element(page.getByText('No active downloads')).toBeVisible();
	});

	it('renders the four base tabs and hides Quarantine from non-admins', async () => {
		render(DownloadQueue);
		await expect.element(page.getByRole('tab', { name: /Active/ })).toBeVisible();
		await expect.element(page.getByRole('tab', { name: /Review/ })).toBeVisible();
		await expect.element(page.getByRole('tab', { name: /Completed/ })).toBeVisible();
		await expect.element(page.getByRole('tab', { name: /Failed/ })).toBeVisible();
		await expect.element(page.getByRole('tab', { name: /Quarantine/ })).not.toBeInTheDocument();
	});

	it('shows the Quarantine tab for admins', async () => {
		h.isAdmin = true;
		render(DownloadQueue);
		await expect.element(page.getByRole('tab', { name: /Quarantine/ })).toBeVisible();
	});

	it('renders an active download in the Active tab', async () => {
		h.items = [task({ id: 'a', album_title: 'OK Computer', status: 'downloading' })];
		render(DownloadQueue);
		await expect.element(page.getByText('OK Computer').first()).toBeVisible();
	});

	it('shows the completed empty state when the Completed tab is selected', async () => {
		render(DownloadQueue);
		await page.getByRole('tab', { name: /Completed/ }).click();
		await expect.element(page.getByText('No completed downloads')).toBeVisible();
	});
});
