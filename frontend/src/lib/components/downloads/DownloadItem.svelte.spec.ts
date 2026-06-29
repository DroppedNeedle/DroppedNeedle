import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DownloadTask } from '$lib/types';

const h = vi.hoisted(() => ({
	cancelMutate: vi.fn(),
	retryMutate: vi.fn(),
	isAdmin: false
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	cancelDownload: () => ({ mutate: h.cancelMutate, isPending: false }),
	retryDownload: () => ({ mutate: h.retryMutate, isPending: false })
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

import DownloadItem from './DownloadItem.svelte';

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

function renderItem(t: DownloadTask) {
	return render(DownloadItem, { props: { task: t } } as Parameters<
		typeof render<typeof DownloadItem>
	>[1]);
}

describe('DownloadItem.svelte', () => {
	beforeEach(() => {
		h.cancelMutate = vi.fn();
		h.retryMutate = vi.fn();
		h.isAdmin = false;
	});

	it('shows the album, a Downloading badge and a Cancel button while downloading', async () => {
		renderItem(task({ status: 'downloading' }));
		await expect.element(page.getByText('OK Computer')).toBeVisible();
		await expect.element(page.getByText('Downloading')).toBeVisible();
		await page.getByRole('button', { name: 'Cancel download' }).click();
		expect(h.cancelMutate).toHaveBeenCalled();
	});

	it('shows a Searching badge for a queued task with no search job', async () => {
		renderItem(task({ status: 'queued', search_job_id: null, candidate_index: null }));
		await expect.element(page.getByText('Searching')).toBeVisible();
	});

	it('shows the error and a Retry button for a failed task', async () => {
		renderItem(task({ status: 'failed', error_message: 'no match found' }));
		await expect.element(page.getByText('no match found')).toBeVisible();
		await page.getByRole('button', { name: 'Retry download' }).click();
		expect(h.retryMutate).toHaveBeenCalled();
	});

	it('shows a "View in Library" link when completed', async () => {
		renderItem(task({ status: 'completed' }));
		await expect.element(page.getByRole('link', { name: 'View in library' })).toBeVisible();
	});
});
