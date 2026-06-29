// lucide-free so the lucide-free nav store can import it. "searching"/"awaiting_review" are derived (backend exposes no search_jobs.status):
//   queued + no search_job_id             -> searching
//   queued + search_job_id + no candidate -> awaiting_review (parked manual tier)
//   otherwise                             -> raw status
import type { DownloadStatus, DownloadTask } from '$lib/types';

export type DerivedDownloadStatus = 'searching' | 'awaiting_review' | DownloadStatus;
export type DownloadTab = 'active' | 'review' | 'completed' | 'failed' | 'quarantine';

export function derivedDownloadStatus(task: DownloadTask): DerivedDownloadStatus {
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
	return task.status === 'failed' || task.status === 'cancelled' || task.status === 'partial';
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

// Collapse auto-retry chains so the queue shows one row per download, not the audit trail
// of superseded attempts. A retry re-runs the same target with retry_count+1, so hide a
// task only when a STRICTLY higher attempt exists for its (type, identity, owner). Tasks at
// the same attempt number are independent downloads (e.g. a re-requested album), not a
// chain, so both are kept. Tasks with no identity (free-text) never collapse.
export function collapseRetryChains(tasks: DownloadTask[]): DownloadTask[] {
	const maxAttempt = new Map<string, number>();
	for (const task of tasks) {
		const key = _retryChainKey(task);
		if (key === null) continue;
		maxAttempt.set(key, Math.max(maxAttempt.get(key) ?? 0, task.retry_count));
	}
	return tasks.filter((task) => {
		const key = _retryChainKey(task);
		return key === null || task.retry_count >= (maxAttempt.get(key) ?? 0);
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
