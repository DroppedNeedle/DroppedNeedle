import { v3 } from '$lib/api/v3/endpoint';

// v3 download-settings URLs (slskd/sabnzbd/indexers/prowlarr/policy/wanted),
// built through the typed registry: every template is a literal the
// contract-coverage gate verifies against the generated spec, and hooks
// import from here so a route rename touches this file only.
// Held/quarantine/search/upgrade surfaces have no v3 HTTP equivalent and
// stay on their v1 builders until that gap is decided.
export const DOWNLOAD_SETTINGS_ENDPOINTS = {
	slskdConfig: () => v3('/api/v3/settings/download-client/config'),
	slskdTest: () => v3('/api/v3/settings/download-client/test'),
	slskdStatus: () => v3('/api/v3/acquire/slskd/status'),
	sabnzbdConfig: () => v3('/api/v3/settings/download-clients/sabnzbd'),
	sabnzbdTest: () => v3('/api/v3/settings/download-clients/sabnzbd/test'),
	sabnzbdStatus: () => v3('/api/v3/acquire/sabnzbd/status'),
	indexers: () => v3('/api/v3/settings/indexers'),
	indexer: (id: string) => v3('/api/v3/settings/indexers/{id}', { path: { id } }),
	indexersReorder: () => v3('/api/v3/settings/indexers/reorder'),
	indexersTest: () => v3('/api/v3/settings/indexers/test'),
	searchBackend: () => v3('/api/v3/settings/indexers/search-backend'),
	prowlarrConfig: () => v3('/api/v3/settings/prowlarr/config'),
	prowlarrTest: () => v3('/api/v3/settings/prowlarr/test'),
	policy: () => v3('/api/v3/settings/download-clients/policy'),
	policySummary: () => v3('/api/v3/settings/download-clients/policy-summary'),
	sourcePriority: () => v3('/api/v3/settings/download-clients/source-priority'),
	wanted: () => v3('/api/v3/settings/download-clients/wanted')
} as const;

// v3 download-task URLs. Only the admin reimport is served so far: it
// imports the task's files right away. The rest stays on v1 builders.
export const DOWNLOAD_TASKS_ENDPOINTS = {
	reimport: (taskId: string) =>
		v3('/api/v3/downloads/tasks/{task_id}/reimport', { path: { task_id: taskId } })
} as const;
