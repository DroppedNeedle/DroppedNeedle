import { derivedDownloadStatus } from '$lib/queries/downloads/downloadStatus';
import type { RequestItem, RequestKind } from '$lib/queries/requests/types';
import type { DownloadTask } from '$lib/types';

export type RequestsTab =
	'active' | 'history' | 'wanted' | 'approvals' | 'auto-download' | 'upgrades';

export function itemKind(item: Pick<RequestItem, 'request_kind'>): RequestKind {
	return item.request_kind === 'track' ? 'track' : 'album';
}

export function requestKey(mbid: string, requestKind: RequestKind): string {
	return `${requestKind}:${mbid}`;
}

export function itemRequestKey(item: Pick<RequestItem, 'musicbrainz_id' | 'request_kind'>): string {
	return requestKey(item.musicbrainz_id, itemKind(item));
}

// The request row reads any download still in the queue as "downloading",
// searching and awaiting review included, so an active card takes its state
// from the linked download when the queue lists it.
export function liveRequestStatus(
	item: Pick<RequestItem, 'status' | 'task_id'>,
	tasks: ReadonlyMap<string, DownloadTask>
): string {
	const task = item.task_id ? tasks.get(item.task_id) : undefined;
	if (!task) return item.status;
	switch (derivedDownloadStatus(task)) {
		case 'searching':
			return 'pending';
		case 'awaiting_review':
			return 'awaiting_review';
		case 'queued':
			return 'queued';
		case 'downloading':
			return 'downloading';
		case 'processing':
			return 'importing';
		default:
			return item.status;
	}
}

export function timeAgo(epochSeconds: number): string {
	const diff = Date.now() / 1000 - epochSeconds;
	if (diff < 60) return 'just now';
	if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
	if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
	return `${Math.floor(diff / 86400)}d ago`;
}
