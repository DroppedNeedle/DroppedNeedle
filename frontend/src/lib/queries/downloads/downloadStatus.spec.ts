import { describe, expect, it } from 'vitest';

import type { DownloadTask } from '$lib/types';

import {
	activeCount,
	bucketDownloads,
	canCancel,
	canRetry,
	collapseRetryChains,
	derivedDownloadStatus,
	formatRetryEta,
	nowPressing,
	retryDisplay,
	tabForTask
} from './downloadStatus';

function task(overrides: Partial<DownloadTask> = {}): DownloadTask {
	return {
		id: 't',
		user_id: 'u',
		download_type: 'album',
		release_group_mbid: 'rg',
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
		next_retry_at: null,
		retry_max: 6,
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

	it('queued with a picked candidate stays "queued" (transient)', () => {
		expect(
			derivedDownloadStatus(task({ status: 'queued', search_job_id: 'j', candidate_index: 0 }))
		).toBe('queued');
	});

	it('passes non-queued statuses through unchanged', () => {
		expect(derivedDownloadStatus(task({ status: 'downloading' }))).toBe('downloading');
		expect(derivedDownloadStatus(task({ status: 'completed' }))).toBe('completed');
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

	it('buckets and sorts most-recent first', () => {
		const a = task({ id: 'a', status: 'downloading', created_at: 1 });
		const b = task({ id: 'b', status: 'downloading', created_at: 2 });
		expect(bucketDownloads([a, b]).active.map((t) => t.id)).toEqual(['b', 'a']);
	});

	it('counts only active tasks', () => {
		expect(
			activeCount([
				task({ status: 'downloading' }),
				task({ status: 'completed' }),
				task({ status: 'queued' })
			])
		).toBe(2);
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

describe('nowPressing', () => {
	it('prefers the most recent downloading/processing task over a newer searching one', () => {
		const dl = task({ id: 'dl', status: 'downloading', created_at: 1 });
		const searching = task({ id: 's', status: 'queued', created_at: 5 });
		expect(nowPressing([dl, searching])?.id).toBe('dl');
	});

	it('falls back to the most recent active task when none are live', () => {
		const s1 = task({ id: 's1', status: 'queued', created_at: 1 });
		const s2 = task({ id: 's2', status: 'queued', created_at: 2 });
		expect(nowPressing([s1, s2])?.id).toBe('s2');
	});

	it('returns null when nothing is active', () => {
		expect(nowPressing([task({ status: 'completed' })])).toBeNull();
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
		expect(retryDisplay(task({ status: 'failed', next_retry_at: null, retry_max: 0 }), NOW)).toBeNull();
		expect(retryDisplay(task({ status: 'downloading', retry_count: 1, retry_max: 0 }), NOW)).toBeNull();
	});
});

describe('formatRetryEta', () => {
	it('formats sub-minute, minutes, and hours', () => {
		expect(formatRetryEta(0.4)).toBe('<1m');
		expect(formatRetryEta(12)).toBe('~12m');
		expect(formatRetryEta(120)).toBe('~2h');
	});
});

describe('collapseRetryChains', () => {
	it('keeps only the latest attempt per (type, identity, owner)', () => {
		const original = task({ id: 'o', status: 'failed', retry_count: 0, release_group_mbid: 'rg1' });
		const retry = task({ id: 'r', status: 'downloading', retry_count: 1, release_group_mbid: 'rg1' });
		const collapsed = collapseRetryChains([original, retry]);
		expect(collapsed).toHaveLength(1);
		expect(collapsed[0].id).toBe('r');
	});

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

	it('keeps two independent downloads of the same album (same attempt) as separate rows', () => {
		// not a retry chain (both retry_count 0) - e.g. a re-requested album; hiding one loses a real task
		const first = task({ id: 'a', status: 'completed', retry_count: 0, release_group_mbid: 'rg1' });
		const second = task({ id: 'b', status: 'downloading', retry_count: 0, release_group_mbid: 'rg1' });
		expect(collapseRetryChains([first, second])).toHaveLength(2);
	});
});
