import { v3 } from '$lib/api/v3/endpoint';

// v3 download-settings URLs (slskd/sabnzbd/indexers/prowlarr/policy/wanted),
// built through the typed registry: every template is a literal the
// contract-coverage gate verifies against the generated spec, and hooks
// import from here so a route rename touches this file only.
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

// v3 download queue URLs: the task list and views, the per-task and bulk
// actions, the activity summary and the admin quarantine list. Live updates
// arrive on the shared event stream (`downloads.changed`,
// `download_progress`), so there is no per-task stream URL.
const byTask = (taskId: string) => ({ path: { task_id: taskId } });

export const DOWNLOAD_TASKS_ENDPOINTS = {
	list: (status?: string, page = 1, pageSize = 100, releaseGroupMbid?: string) =>
		v3('/api/v3/downloads/tasks', {
			query: {
				status: status || undefined,
				release_group_mbid: releaseGroupMbid || undefined,
				page,
				page_size: pageSize
			}
		}),
	task: (taskId: string) => v3('/api/v3/downloads/tasks/{task_id}', byTask(taskId)),
	files: (taskId: string) => v3('/api/v3/downloads/tasks/{task_id}/files', byTask(taskId)),
	cancel: (taskId: string) => v3('/api/v3/downloads/tasks/{task_id}/cancel', byTask(taskId)),
	nextSource: (taskId: string) =>
		v3('/api/v3/downloads/tasks/{task_id}/next-source', byTask(taskId)),
	retry: (taskId: string) => v3('/api/v3/downloads/tasks/{task_id}/retry', byTask(taskId)),
	reimport: (taskId: string) => v3('/api/v3/downloads/tasks/{task_id}/reimport', byTask(taskId)),
	clear: () => v3('/api/v3/downloads/clear'),
	stopAllRetries: () => v3('/api/v3/downloads/stop-all-retries'),
	retryAllFailed: () => v3('/api/v3/downloads/retry-all-failed'),
	activitySummary: () => v3('/api/v3/downloads/activity-summary'),
	quarantine: () => v3('/api/v3/downloads/quarantine'),
	quarantineDelete: (id: number) =>
		v3('/api/v3/downloads/quarantine/{quarantine_id}', { path: { quarantine_id: id } })
} as const;

// v3 manual album search URLs: start a search, read a job and its
// candidates, pick one, dismiss ("none of these, keep watching") or close.
// Progress arrives as `search_job_updated` on the shared event stream.
const byJob = (jobId: string) => ({ path: { job_id: jobId } });

export const DOWNLOAD_SEARCH_ENDPOINTS = {
	start: () => v3('/api/v3/downloads/search/album'),
	job: (jobId: string) => v3('/api/v3/downloads/search/{job_id}', byJob(jobId)),
	pick: (jobId: string) => v3('/api/v3/downloads/search/{job_id}/pick', byJob(jobId)),
	dismiss: (jobId: string) => v3('/api/v3/downloads/search/{job_id}/dismiss', byJob(jobId)),
	cancel: (jobId: string) => v3('/api/v3/downloads/search/{job_id}/cancel', byJob(jobId))
} as const;

// v3 held-import review URLs: the held list (all, or one album's), the
// per-file actions and audio preview, and the per-download actions.
const byHeld = (id: number) => ({ path: { held_id: id } });
const bySourceTask = (taskId: string) => ({ path: { source_task_id: taskId } });

export const DOWNLOAD_HELD_ENDPOINTS = {
	list: (releaseGroupMbid?: string) =>
		v3('/api/v3/downloads/held', {
			query: { release_group_mbid: releaseGroupMbid || undefined }
		}),
	import: (id: number) => v3('/api/v3/downloads/held/{held_id}/import', byHeld(id)),
	discard: (id: number) => v3('/api/v3/downloads/held/{held_id}/discard', byHeld(id)),
	reverify: (id: number) => v3('/api/v3/downloads/held/{held_id}/reverify', byHeld(id)),
	reverifyBulk: () => v3('/api/v3/downloads/held/reverify'),
	audio: (id: number) => v3('/api/v3/downloads/held/{held_id}/audio', byHeld(id)),
	managementRetry: (taskId: string) =>
		v3('/api/v3/downloads/held/management/{source_task_id}/retry', bySourceTask(taskId)),
	managementDiscard: (taskId: string) =>
		v3('/api/v3/downloads/held/management/{source_task_id}/discard', bySourceTask(taskId)),
	verdictDiscard: (taskId: string) =>
		v3('/api/v3/downloads/held/verdict/{source_task_id}/discard', bySourceTask(taskId))
} as const;

// v3 quality-upgrade URLs: the below-cutoff worklist and the album and
// track upgrade asks.
export const DOWNLOAD_UPGRADE_ENDPOINTS = {
	cutoffUnmet: () => v3('/api/v3/downloads/cutoff-unmet'),
	album: () => v3('/api/v3/downloads/upgrade/album'),
	track: () => v3('/api/v3/downloads/upgrade/track')
} as const;
