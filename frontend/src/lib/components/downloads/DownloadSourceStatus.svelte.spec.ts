import { page, userEvent } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DownloadSourceUpdate, DownloadTask } from '$lib/types';

const h = vi.hoisted(() => ({
	mutate: vi.fn(),
	reset: vi.fn(),
	pending: false
}));

vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	tryNextSource: () => ({
		mutate: h.mutate,
		reset: h.reset,
		get isPending() {
			return h.pending;
		}
	})
}));

import DownloadSourceStatus from './DownloadSourceStatus.svelte';

function task(overrides: Partial<DownloadTask> = {}): DownloadTask {
	return {
		id: 'task-1',
		user_id: 'user-1',
		download_type: 'album',
		source: 'soulseek',
		release_group_mbid: 'rg-1',
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: null,
		artist_name: 'Radiohead',
		album_title: 'OK Computer',
		track_title: null,
		year: 1997,
		status: 'downloading',
		progress_percent: 0,
		total_size_bytes: 1000,
		downloaded_bytes: 0,
		files_total: 10,
		files_completed: 0,
		files_failed: 0,
		source_username: 'peer',
		search_job_id: 'job-1',
		candidate_index: 0,
		preflight_score: 0.9,
		final_path: null,
		error_message: null,
		retry_count: 0,
		created_at: 1,
		updated_at: 2,
		completed_at: null,
		next_retry_at: null,
		retry_max: 6,
		retry_ladder_minutes: [15, 30, 60, 120, 240, 480],
		acquisition_cleanup_state: 'in_use',
		quality_format: 'flac',
		quality_bit_depth: 24,
		quality_sample_rate: 48_000,
		advertised_queue_depth: 2710,
		queue_position_start: 91,
		queue_position_end: 100,
		remote_queued: true,
		preferred_quality_fallback_at: Date.now() / 1000 + 13.5 * 60,
		attempt_number: 1,
		attempt_total: 3,
		has_next_source: true,
		held_for_review: false,
		...overrides
	};
}

async function renderStatus(
	download: DownloadTask,
	bytesDownloaded = download.downloaded_bytes,
	live: DownloadSourceUpdate | null = null
) {
	return await render(DownloadSourceStatus, {
		props: { task: download, bytesDownloaded, live }
	} as Parameters<typeof render<typeof DownloadSourceStatus>>[1]);
}

describe('DownloadSourceStatus.svelte', () => {
	beforeEach(() => {
		h.mutate = vi.fn();
		h.reset = vi.fn();
		h.pending = false;
	});

	it('renders quality, queue, attempt, live range and fallback for a queued transfer', async () => {
		await renderStatus(task());

		await expect.element(page.getByText('24-bit / 48 kHz FLAC')).toBeVisible();
		await expect.element(page.getByText('Waiting for Soulseek · queue 2,710')).toBeVisible();
		await expect.element(page.getByText('Trying source 1 of 3')).toBeVisible();
		await expect.element(page.getByText('Live position 91–100')).toBeVisible();
		await expect.element(page.getByText('24-bit/48 kHz lossless fallback in 14m')).toBeVisible();
		await expect.element(page.getByRole('status')).toHaveAttribute('aria-live', 'polite');
	});

	it('invokes next source from the keyboard with the visible candidate index', async () => {
		await renderStatus(task({ candidate_index: 4 }));
		const button = page.getByRole('button', { name: 'Try the next ranked download source' });

		await userEvent.tab();
		await expect.element(button).toHaveFocus();
		await userEvent.keyboard('{Enter}');

		expect(h.mutate).toHaveBeenCalledWith(
			{ id: 'task-1', candidateIndex: 4 },
			expect.objectContaining({ onError: expect.any(Function) })
		);
	});

	it('replaces persisted source details with a live automatic fallback', async () => {
		await renderStatus(task(), 0, {
			candidate_index: 1,
			source: 'soulseek',
			quality_format: 'flac',
			quality_bit_depth: 16,
			quality_sample_rate: 44_100,
			advertised_queue_depth: 0,
			queue_position_start: null,
			queue_position_end: null,
			remote_queued: true,
			preferred_quality_fallback_at: null,
			attempt_number: 2,
			attempt_total: 3,
			has_next_source: true
		});

		await expect.element(page.getByText('16-bit / 44.1 kHz FLAC')).toBeVisible();
		await expect.element(page.getByText('Waiting for Soulseek · queue 0')).toBeVisible();
		await expect.element(page.getByText('Trying source 2 of 3')).toBeVisible();
		await userEvent.click(
			page.getByRole('button', { name: 'Try the next ranked download source' })
		);
		expect(h.mutate).toHaveBeenCalledWith(
			{ id: 'task-1', candidateIndex: 1 },
			expect.objectContaining({ onError: expect.any(Function) })
		);
	});

	it('keeps a failed source switch visible beside the action', async () => {
		h.mutate.mockImplementation((_input, options) => {
			options.onError(new Error('The queued source changed before it could be switched.'));
		});
		await renderStatus(task());

		await userEvent.click(
			page.getByRole('button', { name: 'Try the next ranked download source' })
		);

		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('The queued source changed before it could be switched.');
	});

	it('stops showing Soulseek telemetry after a live switch to another source', async () => {
		await renderStatus(task(), 0, {
			candidate_index: 1,
			source: 'usenet',
			quality_format: null,
			quality_bit_depth: null,
			quality_sample_rate: null,
			advertised_queue_depth: null,
			queue_position_start: null,
			queue_position_end: null,
			remote_queued: false,
			preferred_quality_fallback_at: null,
			attempt_number: 2,
			attempt_total: 3,
			has_next_source: true
		});

		await expect.element(page.getByRole('status')).not.toBeInTheDocument();
	});

	it('shows pending feedback and disables repeated source changes', async () => {
		h.pending = true;
		await renderStatus(task());

		const button = page.getByRole('button', { name: 'Try the next ranked download source' });
		await expect.element(button).toBeDisabled();
		await expect.element(button).toHaveTextContent('Switching…');
	});

	it('distinguishes active bytes from a remote queue and hides fallback actions', async () => {
		await renderStatus(task({ downloaded_bytes: 1 }), 1);

		await expect.element(page.getByText('Downloading from Soulseek')).toBeVisible();
		await expect.element(page.getByText(/fallback in/)).not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: 'Try the next ranked download source' }))
			.not.toBeInTheDocument();
	});

	it('does not call a zero-byte connection queued until the peer confirms it', async () => {
		await renderStatus(task({ remote_queued: false }));

		await expect.element(page.getByText('Downloading from Soulseek')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Try the next ranked download source' }))
			.not.toBeInTheDocument();
	});

	it('hides historical Soulseek telemetry after the download is held for review', async () => {
		await renderStatus(task({ status: 'failed', held_for_review: true }));

		await expect.element(page.getByText('24-bit / 48 kHz FLAC')).not.toBeInTheDocument();
		await expect.element(page.getByText('Downloading from Soulseek')).not.toBeInTheDocument();
		await expect.element(page.getByText('Trying source 1 of 3')).not.toBeInTheDocument();
	});

	it('names the awaited preference from advertised lossy bitrate', async () => {
		await renderStatus(
			task({
				quality_format: 'mp3',
				quality_bit_depth: null,
				quality_sample_rate: null,
				quality_bitrate: 320
			})
		);

		await expect.element(page.getByText('Lossy 320 fallback in 14m')).toBeVisible();
	});

	it('states explicitly that no further fallback is accepted without a deadline', async () => {
		await renderStatus(task({ preferred_quality_fallback_at: null, has_next_source: false }));

		await expect.element(page.getByText('No fallback accepted')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Try the next ranked download source' }))
			.not.toBeInTheDocument();
	});

	it('renders the display-only awaiting-review state with a candidates link', async () => {
		await renderStatus(task({ status: 'queued', search_job_id: 'job-9', candidate_index: null }));

		await expect
			.element(
				page.getByText('Waiting: only unknown-resolution copies were found - review required')
			)
			.toBeVisible();
		const link = page.getByRole('link', { name: 'Review candidates' });
		await expect.element(link).toBeVisible();
		await expect.element(link).toHaveAttribute('href', '/downloads');
	});

	it('hides the awaiting-review card once a candidate is picked or held', async () => {
		await renderStatus(task({ status: 'queued', candidate_index: 0 }));
		await expect
			.element(page.getByText(/unknown-resolution copies were found/))
			.not.toBeInTheDocument();
	});

	it('shows quality evidence chips and the verbatim snapshot summary', async () => {
		await renderStatus(
			task({
				quality_bitrate: 320,
				quality_preference_step: 1,
				quality_certainty: 'inferred',
				quality_provenance: 'release_title',
				manual_quality_override: true,
				quality_snapshot_summary:
					'Lossless preferred up to 24-bit/192 kHz; lossy target 320 kbps within bounds.'
			})
		);

		await expect.element(page.getByText('320 kbps', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Fallback 1', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Certainty: Inferred', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Provenance: release title', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('Manual pick', { exact: true })).toBeVisible();
		await expect
			.element(
				page.getByTitle(
					'Lossless preferred up to 24-bit/192 kHz; lossy target 320 kbps within bounds.'
				)
			)
			.toBeVisible();
	});
});
