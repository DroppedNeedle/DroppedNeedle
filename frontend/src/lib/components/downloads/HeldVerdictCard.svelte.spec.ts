import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DownloadTask, HeldImport } from '$lib/types';

const h = vi.hoisted(() => ({
	discardVerdict: vi.fn(),
	retry: vi.fn(),
	importMut: vi.fn(),
	discardMut: vi.fn(),
	reverifyMut: vi.fn()
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	discardHeldVerdict: () => ({ mutate: h.discardVerdict, isPending: false }),
	retryDownload: () => ({ mutate: h.retry, isPending: false }),
	importHeldTrack: () => ({ mutate: h.importMut, isPending: false, error: null }),
	discardHeldTrack: () => ({ mutate: h.discardMut, isPending: false, error: null }),
	reverifyHeldTrack: () => ({ mutate: h.reverifyMut, isPending: false, error: null })
}));

import HeldVerdictCard from './HeldVerdictCard.svelte';

function task(): DownloadTask {
	return {
		id: 'task-1',
		user_id: 'user-a',
		download_type: 'album',
		source: 'soulseek',
		release_group_mbid: 'rg-1',
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: null,
		artist_name: 'Poppy',
		album_title: 'Flux - Sessions',
		track_title: null,
		year: 2021,
		status: 'failed',
		progress_percent: 100,
		total_size_bytes: null,
		downloaded_bytes: 0,
		files_total: 9,
		files_completed: 0,
		files_failed: 9,
		source_username: 'peerA',
		search_job_id: null,
		candidate_index: 3,
		preflight_score: 0.84,
		final_path: null,
		error_message: null,
		retry_count: 0,
		created_at: 1,
		updated_at: 2,
		completed_at: 3,
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
		preferred_quality_fallback_at: null,
		attempt_number: 3,
		attempt_total: 3,
		has_next_source: false,
		held_for_review: true,
		wrong_product_verdict_at: 4,
		wrong_product_detail: '2021. Flux'
	};
}

function held(track: number): HeldImport {
	return {
		id: track,
		release_group_mbid: 'rg-1',
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: `recording-${track}`,
		track_number: track,
		disc_number: 1,
		track_title: `Track ${track}`,
		artist_name: 'Poppy',
		album_title: 'Flux - Sessions',
		year: 2021,
		original_filename: `${track}.flac`,
		file_format: 'flac',
		duration_seconds: 180,
		expected_duration_seconds: 132,
		reason: 'tag_mismatch',
		reason_detail: null,
		source: 'soulseek',
		source_task_id: 'task-1',
		created_at: track,
		evidence_title: 'Flux',
		evidence_artist: 'Poppy',
		evidence_score: null,
		management_retry_count: 0,
		management_next_retry_at: null
	};
}

describe('HeldVerdictCard.svelte', () => {
	beforeEach(() => {
		h.discardVerdict = vi.fn();
		h.retry = vi.fn();
		h.importMut = vi.fn();
		h.discardMut = vi.fn();
		h.reverifyMut = vi.fn();
	});

	it('presents a wrong-product grab as one actionable unit', async () => {
		await render(HeldVerdictCard, {
			props: { task: task(), items: [held(2), held(1)] }
		} as Parameters<typeof render<typeof HeldVerdictCard>>[1]);

		await expect.element(page.getByText('Wrong edition grabbed')).toBeVisible();
		await expect.element(page.getByText('2 files held, none imported')).toBeVisible();
		await expect.element(page.getByText(/Grabbed “2021\. Flux”/)).toBeVisible();
	});

	it('expands to the member tracks for individual review', async () => {
		await render(HeldVerdictCard, {
			props: { task: task(), items: [held(2), held(1)] }
		} as Parameters<typeof render<typeof HeldVerdictCard>>[1]);

		await page.getByRole('button', { name: 'Review tracks one by one' }).click();
		await expect.element(page.getByText('Track 1')).toBeVisible();
		await expect.element(page.getByText('Track 2')).toBeVisible();
	});

	it('retries the download from the card', async () => {
		await render(HeldVerdictCard, { props: { task: task(), items: [held(1)] } } as Parameters<
			typeof render<typeof HeldVerdictCard>
		>[1]);

		await page.getByRole('button', { name: 'Retry download' }).click();
		expect(h.retry).toHaveBeenCalledWith('task-1');
	});

	it('requires confirmation before discarding every held file', async () => {
		await render(HeldVerdictCard, {
			props: { task: task(), items: [held(1), held(2)] }
		} as Parameters<typeof render<typeof HeldVerdictCard>>[1]);

		await page.getByRole('button', { name: 'Discard all 2' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Discard these held files?' }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Discard held files' }).click();
		expect(h.discardVerdict).toHaveBeenCalledWith(
			{ taskId: 'task-1' },
			expect.objectContaining({ onSuccess: expect.any(Function) })
		);
	});
});
