import { get } from 'svelte/store';
import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, onTestFinished } from 'vitest';

import { toAuthUser } from '$lib/features/auth/queries/types';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import type { DownloadTask, HeldEntry, HeldFile } from '$lib/features/downloads/queries/types';

import DownloadQueue from './DownloadQueue.svelte';

const year = new Date().getFullYear();
const DOWNLOAD_EXPIRES = new Date(year, 5, 23).getTime();
const UPLOAD_EXPIRES = new Date(year, 6, 4).getTime();

function file(id: number, over: Partial<HeldFile> = {}): HeldFile {
	return {
		id,
		original_name: `Drift/${id}.flac`,
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: null,
		disc_number: 1,
		track_number: id,
		track_title: `Track ${id}`,
		file_format: 'flac',
		duration_seconds: 180,
		expected_duration_seconds: null,
		evidence_title: null,
		evidence_artist: null,
		evidence_score: null,
		reason: 'tag_mismatch',
		reason_text: 'Its tags name another album.',
		reason_action: 'Listen, then import or discard it.',
		reason_kind: 'hold',
		reason_detail: null,
		source: 'soulseek',
		source_task_id: 'task-1',
		created_at: 1,
		actions: ['import'],
		...over
	};
}

const download: HeldEntry = {
	id: 1,
	origin: 'download',
	state: 'held',
	release_group_mbid: null,
	recording_mbid: null,
	drop_item_id: null,
	artist_name: 'Vela',
	album_title: 'Drift',
	created_at: 1,
	expires_at: DOWNLOAD_EXPIRES,
	files: [file(1), file(2)]
};

const upload: HeldEntry = {
	id: 2,
	origin: 'upload',
	state: 'held',
	release_group_mbid: null,
	recording_mbid: null,
	drop_item_id: 7,
	artist_name: 'Orla',
	album_title: 'Tidewater',
	created_at: 2,
	expires_at: UPLOAD_EXPIRES,
	files: [
		file(3, { source: 'upload', source_task_id: null }),
		file(4, { source: 'upload', source_task_id: null })
	]
};

function task(id: string, over: Partial<DownloadTask> = {}): DownloadTask {
	return {
		id,
		user_id: 'curator-1',
		download_type: 'album',
		origin: 'user',
		source: 'soulseek',
		release_group_mbid: `rg-${id}`,
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: null,
		artist_name: 'Vela',
		album_title: `Album ${id}`,
		track_title: null,
		year: 2020,
		status: 'cancelled',
		progress_percent: 0,
		total_size_bytes: null,
		downloaded_bytes: 0,
		files_total: 0,
		files_completed: 0,
		files_failed: 0,
		source_username: null,
		search_job_id: null,
		candidate_index: null,
		final_path: null,
		retry_count: 0,
		created_at: 1,
		updated_at: 1,
		completed_at: null,
		next_retry_at: null,
		retry_max: 0,
		retry_ladder_minutes: [],
		acquisition_cleanup_state: 'not_tracked',
		quality_format: null,
		quality_bit_depth: null,
		quality_sample_rate: null,
		advertised_queue_depth: null,
		queue_position_start: null,
		queue_position_end: null,
		remote_queued: false,
		attempt_number: 0,
		attempt_total: 0,
		has_next_source: false,
		held_for_review: false,
		...over
	};
}

const settled = (code: 'already_in_library' | 'already_downloading') => ({
	code,
	kind: 'settled' as const,
	text: code === 'already_in_library' ? 'Already in your library.' : 'Already downloading.',
	action: 'Nothing to do.'
});

const keptUntil = (ms: number) =>
	`Kept until ${new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })}`;

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'curator-1',
			display_name: 'Curator',
			role: 'trusted',
			email: null,
			avatar_url: null,
			username: 'curator',
			username_display: 'Curator'
		})
	);
});

describe('the held list on the Downloads queue', () => {
	it('shows a held download and a held upload as two entries with the same actions', async () => {
		fakeApi({
			'GET /api/v3/downloads/tasks': { items: [], page: 1, page_size: 100 },
			'GET /api/v3/held': { entries: [download, upload] }
		});
		await renderWithQuery(DownloadQueue, {});

		await expect.element(page.getByText('2 to decide', { exact: true })).toBeVisible();

		for (const [title, expires] of [
			['Drift', DOWNLOAD_EXPIRES],
			['Tidewater', UPLOAD_EXPIRES]
		] as const) {
			const card = page.getByRole('article').filter({ hasText: title });
			await expect.element(card.getByText(keptUntil(expires))).toBeVisible();
			await expect.element(card.getByRole('button', { name: 'Import all' })).toBeVisible();
			await expect.element(card.getByRole('button', { name: 'Match…' })).toBeVisible();
			await expect.element(card.getByRole('button', { name: 'Discard all' })).toBeVisible();
		}
	});
});

describe('finished downloads on the Downloads queue', () => {
	it('files a settled stop with finished work and counts only real misses', async () => {
		fakeApi({
			'GET /api/v3/downloads/tasks': {
				items: [
					task('done', { status: 'completed' }),
					task('owned', { reason: settled('already_in_library') }),
					task('stopped'),
					task('broke', { status: 'failed' })
				],
				page: 1,
				page_size: 100
			},
			'GET /api/v3/held': { entries: [] }
		});
		await renderWithQuery(DownloadQueue, {});

		await expect
			.element(page.getByRole('button', { name: /^History/ }))
			.toHaveTextContent("1 in your crate · 2 didn't make it");
	});

	it("links a stop to another user's download fetching the same album", async () => {
		fakeApi({
			'GET /api/v3/downloads/tasks': {
				items: [
					task('live', {
						user_id: 'listener-2',
						status: 'downloading',
						release_group_mbid: 'RG-1',
						created_at: 2
					}),
					task('waited', { release_group_mbid: 'rg-1', reason: settled('already_downloading') })
				],
				page: 1,
				page_size: 100
			},
			'GET /api/v3/held': { entries: [] }
		});
		await renderWithQuery(DownloadQueue, {});

		await page.getByRole('button', { name: /^History/ }).click();
		const row = page.getByRole('article').filter({ hasText: 'Album waited' });
		await expect
			.element(row.getByRole('link', { name: 'See the download' }))
			.toHaveAttribute('href', '#download-live');
		await expect
			.element(row.getByRole('button', { name: 'Retry download' }))
			.not.toBeInTheDocument();
	});

	it('settles Retry when it joins a download already running', async () => {
		fakeApi({
			'GET /api/v3/downloads/tasks': {
				items: [task('broke', { status: 'failed' })],
				page: 1,
				page_size: 100
			},
			'GET /api/v3/held': { entries: [] },
			'POST /api/v3/downloads/tasks/{task_id}/retry': { success: true, task_id: 'elsewhere' }
		});
		onTestFinished(() => {
			queryClient.clear();
			toastStore.hide();
		});
		// The app's client: a retry refreshes the lists there before it is judged.
		await renderWithQuery(DownloadQueue, {}, { client: queryClient });

		await page.getByRole('button', { name: /^History/ }).click();
		const retry = page.getByRole('button', { name: 'Retry download' });
		await retry.click();

		await expect.poll(() => get(toastStore)?.message).toBe('Already downloading.');
		await expect.element(retry).toHaveTextContent(/^\s*Retry\s*$/);
		await expect.element(retry).toBeEnabled();
	});
});
