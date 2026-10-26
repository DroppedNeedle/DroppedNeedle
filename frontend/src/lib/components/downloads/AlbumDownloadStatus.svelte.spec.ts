import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';

import { renderWithQuery } from '$lib/test/render';
import type { DownloadTask, HeldFile } from '$lib/types';

import AlbumDownloadStatus from './AlbumDownloadStatus.svelte';

function task(overrides: Partial<DownloadTask> = {}): DownloadTask {
	return {
		id: 't',
		user_id: 'u',
		download_type: 'album',
		origin: 'user',
		source: 'soulseek',
		release_group_mbid: 'rg',
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: null,
		artist_name: 'A',
		album_title: 'B',
		track_title: null,
		year: 2020,
		status: 'partial',
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

function held(id: number): HeldFile {
	return {
		id,
		original_name: `B/${id}.flac`,
		release_mbid: null,
		release_track_mbid: null,
		recording_mbid: null,
		track_number: id,
		disc_number: 1,
		track_title: `Track ${id}`,
		file_format: 'flac',
		duration_seconds: 180,
		expected_duration_seconds: null,
		reason: 'target_occupied',
		reason_detail: null,
		reason_text: 'Another file already sits where this would go in your library.',
		reason_action: 'Compare it with the one in your library.',
		reason_kind: 'library',
		source: 'soulseek',
		source_task_id: 't',
		created_at: id,
		evidence_title: null,
		evidence_artist: null,
		evidence_score: null,
		actions: ['retry']
	};
}

describe('AlbumDownloadStatus.svelte', () => {
	it('counts the held files of the download and says why', async () => {
		await renderWithQuery(AlbumDownloadStatus, {
			task: task(),
			heldFiles: [held(1), held(2)]
		});

		await expect.element(page.getByText('2 files held', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Another file already sits where this would go in your library.'))
			.toBeVisible();
	});
});
