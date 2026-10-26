import { userIdSegment } from '../userKeySegment';
import { getDownloadScope } from './downloadScope.svelte';

export const DownloadQueryKeyFactory = {
	all: ['downloads'] as const,
	clientConfig: () => [...DownloadQueryKeyFactory.all, 'client-config'] as const,
	clientStatus: () => [...DownloadQueryKeyFactory.all, 'client-status'] as const,
	searchJob: (userId: string | null | undefined, jobId: string) =>
		[...DownloadQueryKeyFactory.all, 'search', userIdSegment(userId), jobId] as const,
	// Every scope (role and generation) of one user's task keys.
	tasksPrefix: (userId: string | null | undefined) =>
		[...DownloadQueryKeyFactory.all, 'tasks', userIdSegment(userId)] as const,
	tasks: (userId?: string) =>
		[
			...DownloadQueryKeyFactory.all,
			'tasks',
			userIdSegment(userId),
			getDownloadScope().role,
			getDownloadScope().generation
		] as const,
	activity: (userId?: string) => [...DownloadQueryKeyFactory.tasks(userId), 'activity'] as const,
	// nested under tasks() so the existing invalidateTasks() prefix-invalidates this too
	albumTasks: (userId: string | null | undefined, mbid: string) =>
		[...DownloadQueryKeyFactory.tasks(userId ?? undefined), 'album', mbid] as const,
	quarantine: () => [...DownloadQueryKeyFactory.all, 'quarantine'] as const,
	// nested under tasks() so invalidateTasks() prefix-invalidates every held list
	heldPrefix: (userId?: string) => [...DownloadQueryKeyFactory.tasks(userId), 'held'] as const,
	held: (
		userId: string | undefined,
		filters: { releaseGroupMbid?: string; taskId?: string; all?: boolean }
	) =>
		[
			...DownloadQueryKeyFactory.heldPrefix(userId),
			{
				release_group_mbid: filters.releaseGroupMbid ?? null,
				task_id: filters.taskId ?? null,
				all: filters.all ?? false
			}
		] as const,
	indexers: () => [...DownloadQueryKeyFactory.all, 'indexers'] as const,
	searchBackend: () => [...DownloadQueryKeyFactory.indexers(), 'search-backend'] as const,
	prowlarr: () => [...DownloadQueryKeyFactory.all, 'prowlarr'] as const,
	sabnzbd: () => [...DownloadQueryKeyFactory.all, 'sabnzbd'] as const,
	sourcePriority: () => [...DownloadQueryKeyFactory.all, 'source-priority'] as const,
	sabnzbdStatus: () => [...DownloadQueryKeyFactory.all, 'sabnzbd-status'] as const,
	policy: () => [...DownloadQueryKeyFactory.all, 'policy'] as const,
	policySummary: () => [...DownloadQueryKeyFactory.all, 'policy-summary'] as const,
	wantedSettings: () => [...DownloadQueryKeyFactory.all, 'wanted-settings'] as const,
	cutoffUnmet: () => [...DownloadQueryKeyFactory.all, 'cutoff-unmet'] as const
};
