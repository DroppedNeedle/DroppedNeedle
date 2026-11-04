import { describe, expect, it } from 'vitest';

import type { DownloadTask } from '$lib/types';

import {
	canCancel,
	canReimport,
	canRetry,
	collapseRetryChains,
	derivedDownloadStatus,
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
		attempt_number: 0,
		attempt_total: 0,
		has_next_source: false,
		held_for_review: false,
		...overrides
	};
}

const NOW = 1_000_000;

describe('derivedDownloadStatus', () => {
	it('derives searching, review and a held task that offers no retry', () => {
		const cases: [Partial<DownloadTask>, string][] = [
			[{ status: 'queued', search_job_id: null }, 'searching'],
			[{ status: 'queued', search_job_id: 'j', candidate_index: null }, 'awaiting_review'],
			// A secured management hold reads as review and never offers a source retry.
			[{ status: 'failed', held_for_review: true }, 'awaiting_review']
		];
		for (const [overrides, expected] of cases) {
			expect(derivedDownloadStatus(task(overrides)), JSON.stringify(overrides)).toBe(expected);
		}
		const held = task({ status: 'failed', held_for_review: true });
		expect(canRetry(held)).toBe(false);
		expect(canReimport(held)).toBe(false);
		expect(retryDisplay(held)).toBeNull();
	});
});

describe('tabForTask', () => {
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
	it('cancels while searching, queued or downloading, and retries only failed, cancelled or partial', () => {
		const cancel: [Partial<DownloadTask>, boolean][] = [
			[{ status: 'queued' }, true],
			[{ status: 'queued', search_job_id: 'j', candidate_index: 0 }, true],
			[{ status: 'downloading' }, true],
			[{ status: 'processing' }, false],
			[{ status: 'completed' }, false]
		];
		for (const [overrides, expected] of cancel) {
			expect(canCancel(task(overrides)), JSON.stringify(overrides)).toBe(expected);
		}
		const retry: [DownloadTask['status'], boolean][] = [
			['failed', true],
			['cancelled', true],
			['partial', true],
			['downloading', false]
		];
		for (const [status, expected] of retry) {
			expect(canRetry(task({ status })), status).toBe(expected);
		}
	});
});

describe('canReimport', () => {
	it('reimports a failed or partial task that picked a candidate from a known peer or Usenet', () => {
		const picked = { search_job_id: 'j', candidate_index: 0 };
		const cases: [Partial<DownloadTask>, boolean][] = [
			[{ status: 'failed', ...picked, source_username: 'peer' }, true],
			[
				{ status: 'partial', search_job_id: 'j', candidate_index: 2, source_username: 'peer' },
				true
			],
			// Never picked a candidate.
			[{ status: 'failed', search_job_id: null, candidate_index: null }, false],
			// A Usenet task's username is always empty.
			[{ status: 'failed', source: 'usenet', ...picked, source_username: '' }, true],
			// A Soulseek task with no username cannot be reimported.
			[{ status: 'failed', source: 'soulseek', ...picked, source_username: '' }, false],
			[{ status: 'failed', ...picked, source_username: null }, false]
		];
		for (const [overrides, expected] of cases) {
			expect(canReimport(task(overrides)), JSON.stringify(overrides)).toBe(expected);
		}
	});
});

describe('retryDisplay', () => {
	it('says retrying, scheduled or exhausted, and nothing for a plain task or with auto-retry off', () => {
		const cases: [Partial<DownloadTask>, ReturnType<typeof retryDisplay>][] = [
			[
				{ status: 'downloading', retry_count: 2, retry_max: 6 },
				{ kind: 'retrying', attempt: 2, max: 6 }
			],
			[
				{ status: 'failed', retry_count: 1, next_retry_at: NOW + 12 * 60 },
				{ kind: 'scheduled', etaMinutes: 12 }
			],
			[{ status: 'failed', retry_count: 6, next_retry_at: null }, { kind: 'failed_exhausted' }],
			[{ status: 'downloading', retry_count: 0 }, null],
			[{ status: 'partial', next_retry_at: null }, null],
			// Auto-retry off: a plain status, never "out of retries" or "/0".
			[{ status: 'failed', next_retry_at: null, retry_max: 0 }, null],
			[{ status: 'downloading', retry_count: 1, retry_max: 0 }, null]
		];
		for (const [overrides, expected] of cases) {
			expect(retryDisplay(task(overrides), NOW), JSON.stringify(overrides)).toEqual(expected);
		}
	});
});

describe('isWanted + sectionForTask', () => {
	it('wants a failed or partial task with a scheduled retry, and files each task in its section', () => {
		const scheduled = { retry_count: 1, next_retry_at: NOW + 600 };
		const wanted: [Partial<DownloadTask>, boolean][] = [
			[{ status: 'failed', ...scheduled }, true],
			[{ status: 'partial', ...scheduled }, true],
			[{ status: 'failed', next_retry_at: null }, false]
		];
		for (const [overrides, expected] of wanted) {
			expect(isWanted(task(overrides), NOW), JSON.stringify(overrides)).toBe(expected);
		}
		const sections: [Partial<DownloadTask>, string][] = [
			[{ status: 'downloading' }, 'now_spinning'],
			[{ status: 'processing' }, 'now_spinning'],
			[{ status: 'queued', search_job_id: null }, 'cueing'],
			[{ status: 'queued', search_job_id: 'j', candidate_index: null }, 'needs_you'],
			[{ status: 'failed', ...scheduled }, 'wanted'],
			[{ status: 'completed' }, 'history'],
			// Out of retries, and done with no retry.
			[{ status: 'failed', next_retry_at: null }, 'history'],
			[{ status: 'partial', next_retry_at: null }, 'history']
		];
		for (const [overrides, expected] of sections) {
			expect(sectionForTask(task(overrides), NOW), JSON.stringify(overrides)).toBe(expected);
		}
	});
});

describe('retryLadderState', () => {
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
	it('folds only attempts of one album by one user, newest first, never identity-less tasks', () => {
		// Different albums, users or types stay apart.
		expect(
			collapseRetryChains([
				task({ id: 'a', release_group_mbid: 'rg1', user_id: 'u1' }),
				task({ id: 'b', release_group_mbid: 'rg2', user_id: 'u1' }),
				task({ id: 'c', release_group_mbid: 'rg1', user_id: 'u2' })
			])
		).toHaveLength(3);
		// Free-text tasks have no identity to fold on.
		expect(
			collapseRetryChains([
				task({ id: 'a', release_group_mbid: '', recording_mbid: null }),
				task({ id: 'b', release_group_mbid: '', recording_mbid: null })
			])
		).toHaveLength(2);
		// A re-request resets retry_count to 0, so a stale failed retry-7 task must not
		// outrank, and hide, today's retry-0 download: the newest created_at wins.
		const collapsed = collapseRetryChains([
			task({
				id: 'old',
				status: 'failed',
				retry_count: 7,
				created_at: 100,
				release_group_mbid: 'rg1'
			}),
			task({
				id: 'new',
				status: 'downloading',
				retry_count: 0,
				created_at: 200,
				release_group_mbid: 'rg1'
			})
		]);
		expect(collapsed).toHaveLength(1);
		expect(collapsed[0].id).toBe('new');
	});
});
