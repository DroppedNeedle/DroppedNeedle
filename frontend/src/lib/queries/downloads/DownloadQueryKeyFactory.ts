import { getDownloadScope } from './downloadScope.svelte';

export const DownloadQueryKeyFactory = {
	all: ['downloads'] as const,
	clientConfig: () => [...DownloadQueryKeyFactory.all, 'client-config'] as const,
	clientStatus: () => [...DownloadQueryKeyFactory.all, 'client-status'] as const,
	searchJob: (userId: string | undefined, jobId: string) =>
		[...DownloadQueryKeyFactory.all, 'search', userId ?? 'anon', jobId] as const,
	tasks: (userId?: string) =>
		[
			...DownloadQueryKeyFactory.all,
			'tasks',
			userId ?? 'anon',
			getDownloadScope().role,
			getDownloadScope().generation
		] as const,
	activity: (userId?: string) => [...DownloadQueryKeyFactory.tasks(userId), 'activity'] as const,
	// nested under tasks() so the existing invalidateTasks() prefix-invalidates this too
	albumTasks: (userId: string | undefined, mbid: string) =>
		[...DownloadQueryKeyFactory.tasks(userId), 'album', mbid] as const,
	quarantine: () => [...DownloadQueryKeyFactory.all, 'quarantine'] as const,
	// nested under tasks() so invalidateTasks() prefix-invalidates held lists (all + per-album)
	heldPrefix: (userId?: string) => [...DownloadQueryKeyFactory.tasks(userId), 'held'] as const,
	held: (userId?: string, mbid?: string) =>
		[...DownloadQueryKeyFactory.heldPrefix(userId), mbid ?? 'all'] as const,
	indexers: () => [...DownloadQueryKeyFactory.all, 'indexers'] as const,
	searchBackend: () => [...DownloadQueryKeyFactory.indexers(), 'search-backend'] as const,
	prowlarr: () => [...DownloadQueryKeyFactory.all, 'prowlarr'] as const,
	sabnzbd: () => [...DownloadQueryKeyFactory.all, 'sabnzbd'] as const,
	sabnzbdStatus: () => [...DownloadQueryKeyFactory.all, 'sabnzbd-status'] as const,
	nzbget: () => [...DownloadQueryKeyFactory.all, 'nzbget'] as const,
	nzbgetStatus: () => [...DownloadQueryKeyFactory.all, 'nzbget-status'] as const,
	policy: () => [...DownloadQueryKeyFactory.all, 'policy'] as const,
	policySummary: () => [...DownloadQueryKeyFactory.all, 'policy-summary'] as const,
	wantedSettings: () => [...DownloadQueryKeyFactory.all, 'wanted-settings'] as const,
	cutoffUnmet: () => [...DownloadQueryKeyFactory.all, 'cutoff-unmet'] as const
};
