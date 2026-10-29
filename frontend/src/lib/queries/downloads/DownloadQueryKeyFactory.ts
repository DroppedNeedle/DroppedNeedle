import { ADMIN, key, SHARED, userScope } from '../keys';
import { getDownloadScope } from './downloadScope.svelte';

type UserId = string | null | undefined;

export const DownloadQueryKeyFactory = {
	feature: 'downloads',
	clientConfig: () => key(ADMIN, 'downloads', 'client-config'),
	clientStatus: () => key(SHARED, 'downloads', 'client-status'),
	searchJob: (userId: UserId, jobId: string) =>
		key(userScope(userId), 'downloads', 'search', jobId),
	// Every role and generation of one user's task keys.
	tasksPrefix: (userId: UserId) => key(userScope(userId), 'downloads', 'tasks'),
	// The role and its generation: a role change starts new keys, so an answer
	// read under the old role is never shown under the new one.
	tasks: (userId: UserId) =>
		[
			...DownloadQueryKeyFactory.tasksPrefix(userId),
			getDownloadScope().role,
			getDownloadScope().generation
		] as const,
	activity: (userId: UserId) => [...DownloadQueryKeyFactory.tasks(userId), 'activity'] as const,
	// Nested under tasks() so invalidating the tasks refreshes these too.
	albumTasks: (userId: UserId, mbid: string) =>
		[...DownloadQueryKeyFactory.tasks(userId), 'album', mbid] as const,
	editionFetch: (userId: UserId, group: string) =>
		[...DownloadQueryKeyFactory.tasks(userId), 'edition', group] as const,
	heldPrefix: (userId: UserId) => [...DownloadQueryKeyFactory.tasks(userId), 'held'] as const,
	held: (userId: UserId, filters: { releaseGroupMbid?: string; taskId?: string; all?: boolean }) =>
		[
			...DownloadQueryKeyFactory.heldPrefix(userId),
			{
				release_group_mbid: filters.releaseGroupMbid ?? null,
				task_id: filters.taskId ?? null,
				all: filters.all ?? false
			}
		] as const,
	quarantine: () => key(ADMIN, 'downloads', 'quarantine'),
	indexers: () => key(ADMIN, 'downloads', 'indexers'),
	searchBackend: () => [...DownloadQueryKeyFactory.indexers(), 'search-backend'] as const,
	prowlarr: () => key(ADMIN, 'downloads', 'prowlarr'),
	sabnzbd: () => key(ADMIN, 'downloads', 'sabnzbd'),
	sourcePriority: () => key(ADMIN, 'downloads', 'source-priority'),
	sabnzbdStatus: () => key(ADMIN, 'downloads', 'sabnzbd-status'),
	policy: () => key(ADMIN, 'downloads', 'policy'),
	policySummary: () => key(SHARED, 'downloads', 'policy-summary'),
	wantedSettings: () => key(ADMIN, 'downloads', 'wanted-settings'),
	cutoffUnmet: () => key(ADMIN, 'downloads', 'cutoff-unmet')
};
