import { v3 } from '$lib/api/v3/endpoint';

// v3 drop-import URLs: the upload, the job list and the review actions
// (match, discard). Progress arrives as `drop_import_updated` on the shared
// event stream.
const byItem = (itemId: number) => ({ path: { item_id: itemId } });

export const DROP_IMPORT_ENDPOINTS = {
	uploads: () => v3('/api/v3/import/uploads'),
	jobs: (all = false) => v3('/api/v3/import/jobs', { query: { all: all || undefined } }),
	match: (itemId: number) => v3('/api/v3/import/items/{item_id}/match', byItem(itemId)),
	discard: (itemId: number) => v3('/api/v3/import/items/{item_id}/discard', byItem(itemId))
} as const;
