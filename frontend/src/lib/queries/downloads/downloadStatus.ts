// lucide-free so the lucide-free nav store can import it. "searching"/"awaiting_review" are derived (backend exposes no search_jobs.status):
//   queued + no search_job_id             -> searching
//   queued + search_job_id + no candidate -> awaiting_review (parked manual tier)
//   otherwise                             -> raw status
import type { DownloadStatus, DownloadTask } from '$lib/types';

export type DerivedDownloadStatus = 'searching' | 'awaiting_review' | DownloadStatus;
export type DownloadTab = 'active' | 'review' | 'completed' | 'failed' | 'quarantine';

export function derivedDownloadStatus(task: DownloadTask): DerivedDownloadStatus {
	if (task.held_for_review) return 'awaiting_review';
	if (task.status === 'queued') {
		if (!task.search_job_id) return 'searching';
		if (task.candidate_index === null || task.candidate_index === undefined) {
			return 'awaiting_review';
		}
	}
	return task.status;
}

// mirrors the backend _ACTIVE_STATUSES - a task still in flight (not a terminal state)
const ACTIVE_STATUSES: DownloadStatus[] = ['queued', 'downloading', 'processing'];

export function isActiveDownloadStatus(status: DownloadStatus): boolean {
	return ACTIVE_STATUSES.includes(status);
}

export function hasActiveTask(tasks: DownloadTask[]): boolean {
	return tasks.some((t) => isActiveDownloadStatus(t.status));
}

export type DownloadBucketTab = Exclude<DownloadTab, 'quarantine'>;

export function tabForTask(task: DownloadTask): DownloadBucketTab {
	const derived = derivedDownloadStatus(task);
	if (derived === 'awaiting_review') return 'review';
	if (derived === 'completed' || derived === 'partial') return 'completed';
	if (derived === 'failed' || derived === 'cancelled') return 'failed';
	return 'active';
}

export type DownloadBuckets = Record<DownloadBucketTab, DownloadTask[]>;

export function bucketDownloads(tasks: DownloadTask[]): DownloadBuckets {
	const buckets: DownloadBuckets = { active: [], review: [], completed: [], failed: [] };
	for (const task of tasks) buckets[tabForTask(task)].push(task);
	for (const key of Object.keys(buckets) as DownloadBucketTab[]) {
		buckets[key].sort((a, b) => b.created_at - a.created_at);
	}
	return buckets;
}

export function activeCount(tasks: DownloadTask[]): number {
	let count = 0;
	for (const task of tasks) if (tabForTask(task) === 'active') count++;
	return count;
}

// no cancel during processing: file move is unsafe to interrupt (UX-8)
export function canCancel(task: DownloadTask): boolean {
	const derived = derivedDownloadStatus(task);
	return derived === 'searching' || derived === 'queued' || derived === 'downloading';
}

export function canRetry(task: DownloadTask): boolean {
	if (task.held_for_review) return false;
	return task.status === 'failed' || task.status === 'cancelled' || task.status === 'partial';
}

export function canReimport(task: DownloadTask): boolean {
	if (task.held_for_review) return false;
	if (!(task.status === 'failed' || task.status === 'partial')) return false;
	if (task.search_job_id == null || task.candidate_index == null) return false;
	// Mirrors the backend reimport guard: Soulseek needs the picked username, while
	// Usenet reimports from the journaled SABnzbd handle (its username is always "").
	if (task.source === 'usenet') return true;
	return task.source_username != null && task.source_username !== '';
}

export type RetryDisplay =
	| { kind: 'scheduled'; etaMinutes: number }
	| { kind: 'retrying'; attempt: number; max: number }
	| { kind: 'failed_exhausted' }
	| null;

// The retry treatment for a (collapsed) task, or null for a normal status:
//   retrying         - a retry is actively re-running (attempt N of the configured max)
//   scheduled        - failed/partial, waiting for the next auto-retry (etaMinutes away)
//   failed_exhausted - failed with no auto-retry left (disabled or attempts used up)
export function retryDisplay(
	task: DownloadTask,
	nowSeconds: number = Date.now() / 1000
): RetryDisplay {
	if (task.held_for_review) return null;
	// No auto-retry configured (retry_max 0): no retry treatment at all - a failed task is
	// just "Failed" and a manual re-run shows its normal status, never "attempt N/0".
	if (task.retry_max <= 0) return null;
	if (task.retry_count > 0 && isActiveDownloadStatus(task.status)) {
		return { kind: 'retrying', attempt: task.retry_count, max: task.retry_max };
	}
	if (task.status === 'failed' || task.status === 'partial') {
		if (task.next_retry_at != null) {
			return { kind: 'scheduled', etaMinutes: Math.max(0, (task.next_retry_at - nowSeconds) / 60) };
		}
		if (task.status === 'failed') return { kind: 'failed_exhausted' };
	}
	return null;
}

// "~12m" / "<1m" / "~3h" - the wait until the next scheduled retry.
export function formatRetryEta(minutes: number): string {
	if (minutes < 1) return '<1m';
	if (minutes > 90) return `~${Math.round(minutes / 60)}h`;
	return `~${Math.round(minutes)}m`;
}

// A live, precise countdown: "11:58" under an hour, "7h 32m" over it.
export function formatCountdown(seconds: number): string {
	const s = Math.max(0, Math.round(seconds));
	if (s >= 3600) {
		// round to whole minutes first, then split - rounding h and m independently could
		// yield an impossible "1h 60m" at the 59.5-minute boundary
		const totalMin = Math.round(s / 60);
		return `${Math.floor(totalMin / 60)}h ${totalMin % 60}m`;
	}
	const m = Math.floor(s / 60);
	const sec = s % 60;
	return `${m}:${String(sec).padStart(2, '0')}`;
}

// The Downloads dashboard groups the queue into meaningful stacked sections (vs the old
// single-tab view), so "what's my turntable doing?" is answerable at a glance:
//   now_spinning - actively downloading/processing (incl. an in-flight retry)
//   wanted       - failed/partial, waiting on the next scheduled auto-retry (the new section)
//   needs_you    - parked for a manual review pick (awaiting_review)
//   cueing       - lined up but not yet pressing (searching / queued)
//   history      - terminal: completed, exhausted-failed, partial-done, cancelled
export type DownloadSection = 'now_spinning' | 'wanted' | 'needs_you' | 'cueing' | 'history';

// A "wanted" album is one the system will re-attempt on a backoff (scheduled, not yet due).
export function isWanted(task: DownloadTask, nowSeconds: number = Date.now() / 1000): boolean {
	return retryDisplay(task, nowSeconds)?.kind === 'scheduled';
}

export function sectionForTask(
	task: DownloadTask,
	nowSeconds: number = Date.now() / 1000
): DownloadSection {
	const derived = derivedDownloadStatus(task);
	if (derived === 'downloading' || derived === 'processing') return 'now_spinning';
	if (derived === 'awaiting_review') return 'needs_you';
	if (isWanted(task, nowSeconds)) return 'wanted';
	if (derived === 'searching' || derived === 'queued') return 'cueing';
	return 'history';
}

export type DownloadSections = Record<DownloadSection, DownloadTask[]>;

export function bucketSections(
	tasks: DownloadTask[],
	nowSeconds: number = Date.now() / 1000
): DownloadSections {
	const sections: DownloadSections = {
		now_spinning: [],
		wanted: [],
		needs_you: [],
		cueing: [],
		history: []
	};
	for (const task of tasks) sections[sectionForTask(task, nowSeconds)].push(task);
	for (const key of Object.keys(sections) as DownloadSection[]) {
		// wanted sorts by soonest next attempt; everything else newest-first
		if (key === 'wanted') {
			sections[key].sort((a, b) => (a.next_retry_at ?? 0) - (b.next_retry_at ?? 0));
		} else {
			sections[key].sort((a, b) => b.created_at - a.created_at);
		}
	}
	return sections;
}

// Live state of a wanted album's backoff ladder, for the cue-countdown card.
export interface RetryLadderState {
	rungs: number[]; // minutes per retry attempt, e.g. [15, 30, 60, 120, 240, 480]
	index: number; // the rung we're currently waiting on (0-based)
	attempt: number; // human "retry N" (index + 1)
	total: number; // total retries on the ladder
	secondsUntilNext: number; // live: seconds to the next attempt
	fractionElapsed: number; // 0..1 progress through this wait (drives the cue ring)
}

export function retryLadderState(
	task: DownloadTask,
	nowSeconds: number = Date.now() / 1000
): RetryLadderState | null {
	const rungs = task.retry_ladder_minutes ?? [];
	if (task.next_retry_at == null || rungs.length === 0) return null;
	const index = Math.min(Math.max(task.retry_count, 0), rungs.length - 1);
	const rungSeconds = Math.max(1, rungs[index] * 60);
	const secondsUntilNext = Math.max(0, task.next_retry_at - nowSeconds);
	const fractionElapsed = Math.min(1, Math.max(0, 1 - secondsUntilNext / rungSeconds));
	return {
		rungs,
		index,
		attempt: index + 1,
		total: rungs.length,
		secondsUntilNext,
		fractionElapsed
	};
}

// Collapse auto-retry chains so the queue shows one row per download, not the audit trail
// of superseded attempts. Keep the NEWEST task per (type, identity, owner) by created_at.
// created_at (not retry_count) is the right key: within a chain each retry is created later,
// AND a fresh re-request starts a NEW chain back at retry_count 0 - so a stale failed
// attempt (e.g. retry 7 from yesterday) must NOT outrank, and hide, today's active retry-0
// download of the same album. Tasks with no identity (free-text) never collapse.
export function collapseRetryChains(tasks: DownloadTask[]): DownloadTask[] {
	const newest = new Map<string, number>();
	for (const task of tasks) {
		const key = _retryChainKey(task);
		if (key === null) continue;
		newest.set(key, Math.max(newest.get(key) ?? -Infinity, task.created_at));
	}
	return tasks.filter((task) => {
		const key = _retryChainKey(task);
		return key === null || task.created_at >= (newest.get(key) ?? -Infinity);
	});
}

function _retryChainKey(task: DownloadTask): string | null {
	const identity = task.download_type === 'track' ? task.recording_mbid : task.release_group_mbid;
	return identity ? `${task.download_type}:${identity}:${task.user_id}` : null;
}

// featured "now pressing": most recent downloading/processing item, else most recent active item
export function nowPressing(tasks: DownloadTask[]): DownloadTask | null {
	const active = tasks.filter((t) => tabForTask(t) === 'active');
	if (active.length === 0) return null;
	const live = active.filter((t) => t.status === 'downloading' || t.status === 'processing');
	const pool = live.length > 0 ? live : active;
	return pool.reduce((best, t) => (t.created_at > best.created_at ? t : best), pool[0]);
}
