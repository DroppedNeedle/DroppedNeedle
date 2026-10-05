import { describe, expect, it } from 'vitest';

import type { DownloadTask } from '$lib/types';

import {
	canCancel,
	canReimport,
	canRetry,
	collapseRetryChains,
	derivedDownloadStatus,
	formatCountdown,
	isWanted,
	retryDisplay,
	retryLadderState,
	sectionForTask,
	tabForTask
} from './downloadStatus';

function task(overrides: Partial<DownloadTask> = {}): DownloadTask {
	return {
		id: 't',
		user_id: 'u',
		download_type: 'album',
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
		preflight_score: null,
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

describe('derivedDownloadStatus', () => {
	it('queued with no search job is "searching"', () => {
		expect(derivedDownloadStatus(task({ status: 'queued', search_job_id: null }))).toBe(
			'searching'
		);
	});

	it('queued with a search job but no picked candidate is "awaiting_review"', () => {
		expect(
			derivedDownloadStatus(task({ status: 'queued', search_job_id: 'j', candidate_index: null }))
		).toBe('awaiting_review');
	});

	it('treats a secured management hold as review and never offers source retry', () => {
		const held = task({ status: 'failed', held_for_review: true });
		expect(derivedDownloadStatus(held)).toBe('awaiting_review');
		expect(canRetry(held)).toBe(false);
		expect(canReimport(held)).toBe(false);
		expect(retryDisplay(held)).toBeNull();
	});
});

describe('tabForTask + bucketDownloads + counts', () => {
	it('routes derived states to the right tab', () => {
		expect(tabForTask(task({ status: 'queued' }))).toBe('active'); // searching
		expect(tabForTask(task({ status: 'downloading' }))).toBe('active');
		expect(tabForTask(task({ status: 'processing' }))).toBe('active');
		expect(tabForTask(task({ status: 'queued', search_job_id: 'j', candidate_index: null }))).toBe(
			'review'
		);
		expect(tabForTask(task({ status: 'completed' }))).toBe('completed');
		expect(tabForTask(task({ status: 'partial' }))).toBe('completed');
		expect(tabForTask(task({ status: 'failed' }))).toBe('failed');
		expect(tabForTask(task({ status: 'cancelled' }))).toBe('failed');
	});
});

describe('canCancel / canRetry', () => {
	it('allows cancel while searching/queued/downloading but not processing', () => {
		expect(canCancel(task({ status: 'queued' }))).toBe(true); // searching
		expect(canCancel(task({ status: 'queued', search_job_id: 'j', candidate_index: 0 }))).toBe(
			true
		);
		expect(canCancel(task({ status: 'downloading' }))).toBe(true);
		expect(canCancel(task({ status: 'processing' }))).toBe(false);
		expect(canCancel(task({ status: 'completed' }))).toBe(false);
	});

	it('allows retry only for failed/cancelled/partial', () => {
		expect(canRetry(task({ status: 'failed' }))).toBe(true);
		expect(canRetry(task({ status: 'cancelled' }))).toBe(true);
		expect(canRetry(task({ status: 'partial' }))).toBe(true);
		expect(canRetry(task({ status: 'downloading' }))).toBe(false);
	});
});

describe('canReimport', () => {
	it('allows reimport for failed/partial tasks that picked a candidate', () => {
		expect(
			canReimport(
				task({ status: 'failed', search_job_id: 'j', candidate_index: 0, source_username: 'peer' })
			)
		).toBe(true);
		expect(
			canReimport(
				task({ status: 'partial', search_job_id: 'j', candidate_index: 2, source_username: 'peer' })
			)
		).toBe(true);
	});

	it('disallows reimport for a task that never picked a candidate', () => {
		expect(
			canReimport(task({ status: 'failed', search_job_id: null, candidate_index: null }))
		).toBe(false);
	});

	it('allows reimport for Usenet tasks whose username is always empty', () => {
		expect(
			canReimport(
				task({
					status: 'failed',
					source: 'usenet',
					search_job_id: 'j',
					candidate_index: 0,
					source_username: ''
				})
			)
		).toBe(true);
	});

	it('disallows reimport for Soulseek tasks with an empty username', () => {
		expect(
			canReimport(
				task({
					status: 'failed',
					source: 'soulseek',
					search_job_id: 'j',
					candidate_index: 0,
					source_username: ''
				})
			)
		).toBe(false);
		expect(
			canReimport(
				task({
					status: 'failed',
					search_job_id: 'j',
					candidate_index: 0,
					source_username: null
				})
			)
		).toBe(false);
	});
});

describe('retryDisplay', () => {
	const NOW = 1_000_000;

	it('is "retrying" while a retry actively re-runs', () => {
		const t = task({ status: 'downloading', retry_count: 2, retry_max: 6 });
		expect(retryDisplay(t, NOW)).toEqual({ kind: 'retrying', attempt: 2, max: 6 });
	});

	it('is "scheduled" with the eta while waiting for the next attempt', () => {
		const t = task({ status: 'failed', retry_count: 1, next_retry_at: NOW + 12 * 60 });
		expect(retryDisplay(t, NOW)).toEqual({ kind: 'scheduled', etaMinutes: 12 });
	});

	it('is "failed_exhausted" for a failed task with no scheduled retry', () => {
		const t = task({ status: 'failed', retry_count: 6, next_retry_at: null });
		expect(retryDisplay(t, NOW)).toEqual({ kind: 'failed_exhausted' });
	});

	it('is null for a normal in-flight (non-retry) task and for partial-without-retry', () => {
		expect(retryDisplay(task({ status: 'downloading', retry_count: 0 }), NOW)).toBeNull();
		expect(retryDisplay(task({ status: 'partial', next_retry_at: null }), NOW)).toBeNull();
	});

	it('is null when auto-retry is off (retry_max 0): plain status, never "out of retries" or "/0"', () => {
		expect(
			retryDisplay(task({ status: 'failed', next_retry_at: null, retry_max: 0 }), NOW)
		).toBeNull();
		expect(
			retryDisplay(task({ status: 'downloading', retry_count: 1, retry_max: 0 }), NOW)
		).toBeNull();
	});
});

describe('formatCountdown', () => {
	it('rolls over cleanly at the hour boundary (never "1h 60m")', () => {
		expect(formatCountdown(7199)).toBe('2h 0m');
		expect(formatCountdown(10770)).toBe('3h 0m');
	});
});

describe('isWanted + sectionForTask + bucketSections', () => {
	const NOW = 1_000_000;

	it('a failed/partial task with a scheduled retry is wanted', () => {
		expect(
			isWanted(task({ status: 'failed', retry_count: 1, next_retry_at: NOW + 600 }), NOW)
		).toBe(true);
		expect(
			isWanted(task({ status: 'partial', retry_count: 1, next_retry_at: NOW + 600 }), NOW)
		).toBe(true);
		expect(isWanted(task({ status: 'failed', next_retry_at: null }), NOW)).toBe(false);
	});

	it('routes each task to its dashboard section', () => {
		expect(sectionForTask(task({ status: 'downloading' }), NOW)).toBe('now_spinning');
		expect(sectionForTask(task({ status: 'processing' }), NOW)).toBe('now_spinning');
		expect(sectionForTask(task({ status: 'queued', search_job_id: null }), NOW)).toBe('cueing'); // searching
		expect(
			sectionForTask(task({ status: 'queued', search_job_id: 'j', candidate_index: null }), NOW)
		).toBe('needs_you');
		expect(
			sectionForTask(task({ status: 'failed', retry_count: 1, next_retry_at: NOW + 600 }), NOW)
		).toBe('wanted');
		expect(sectionForTask(task({ status: 'completed' }), NOW)).toBe('history');
		expect(sectionForTask(task({ status: 'failed', next_retry_at: null }), NOW)).toBe('history'); // exhausted
		expect(sectionForTask(task({ status: 'partial', next_retry_at: null }), NOW)).toBe('history'); // done, no retry
	});
});

describe('retryLadderState', () => {
	const NOW = 1_000_000;

	it('reports the current rung, attempt, live countdown and elapsed fraction', () => {
		// retry_count 1 -> waiting on rung index 1 (30m); 10m left of that 30m wait
		const t = task({
			status: 'failed',
			retry_count: 1,
			next_retry_at: NOW + 600,
			retry_ladder_minutes: [15, 30, 60, 120, 240, 480]
		});
		const state = retryLadderState(t, NOW);
		expect(state).not.toBeNull();
		expect(state!.index).toBe(1);
		expect(state!.attempt).toBe(2);
		expect(state!.total).toBe(6);
		expect(state!.secondsUntilNext).toBe(600);
		expect(state!.fractionElapsed).toBeCloseTo(1 - 600 / (30 * 60), 5);
	});
});

describe('collapseRetryChains', () => {
	it('does not merge different albums, users, or types', () => {
		const a = task({ id: 'a', release_group_mbid: 'rg1', user_id: 'u1' });
		const b = task({ id: 'b', release_group_mbid: 'rg2', user_id: 'u1' });
		const c = task({ id: 'c', release_group_mbid: 'rg1', user_id: 'u2' });
		expect(collapseRetryChains([a, b, c])).toHaveLength(3);
	});

	it('never collapses identity-less (free-text) tasks together', () => {
		const a = task({ id: 'a', release_group_mbid: '', recording_mbid: null });
		const b = task({ id: 'b', release_group_mbid: '', recording_mbid: null });
		expect(collapseRetryChains([a, b])).toHaveLength(2);
	});

	it('a fresh re-request supersedes a stale higher-retry attempt of the same album', () => {
		// The live bug: a re-request resets retry_count to 0, so a stale failed retry-7 task
		// from yesterday must NOT outrank, and hide, today's active retry-0 download. Newest
		// created_at wins - the higher retry_count does not.
		const stale = task({
			id: 'old',
			status: 'failed',
			retry_count: 7,
			created_at: 100,
			release_group_mbid: 'rg1'
		});
		const fresh = task({
			id: 'new',
			status: 'downloading',
			retry_count: 0,
			created_at: 200,
			release_group_mbid: 'rg1'
		});
		const collapsed = collapseRetryChains([stale, fresh]);
		expect(collapsed).toHaveLength(1);
		expect(collapsed[0].id).toBe('new');
	});
});
