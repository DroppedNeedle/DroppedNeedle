import { v3 } from '$lib/api/v3/endpoint';

// v3 drop-import URLs: the upload and the job list. An item that needs a
// decision is a held entry, acted on through the held routes. Progress
// arrives as `drop_import_updated` on the shared event stream.
export const DROP_IMPORT_ENDPOINTS = {
	uploads: () => v3('/api/v3/import/uploads'),
	jobs: (all = false) => v3('/api/v3/import/jobs', { query: { all: all || undefined } })
} as const;
