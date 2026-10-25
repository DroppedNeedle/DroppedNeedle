import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { DownloadTask } from '$lib/types';

import DownloadReasonNote from './DownloadReasonNote.svelte';

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
		status: 'queued',
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

describe('DownloadReasonNote', () => {
	it('says a queued task waits for a download source', async () => {
		render(DownloadReasonNote, {
			task: task({
				reason: {
					code: 'no_download_source',
					kind: 'wait',
					text: 'Waiting: no download source configured.',
					action: 'Turn on slskd, SABnzbd or a plugin source in Settings, Downloads.'
				}
			})
		});

		await expect
			.element(page.getByText('Waiting: no download source configured.'))
			.toHaveClass(/text-warning/);
	});
});
