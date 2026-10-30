import type { RequestItem } from './types';

// The request row reads any download still in the queue as "downloading",
// searching and awaiting review included, so an active card takes its state
// from the linked download's own state, which the server sends on the row
// whoever started that download.
export function liveRequestStatus(item: Pick<RequestItem, 'status' | 'task_state'>): string {
	switch (item.task_state) {
		case 'searching':
			return 'pending';
		case 'awaiting_review':
			return 'awaiting_review';
		case 'queued':
			return 'queued';
		case 'downloading':
			return 'downloading';
		case 'importing':
			return 'importing';
		default:
			return item.status;
	}
}
