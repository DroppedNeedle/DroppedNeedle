export const DownloadQueryKeyFactory = {
	all: ['downloads'] as const,
	clientConfig: () => [...DownloadQueryKeyFactory.all, 'client-config'] as const,
	clientStatus: () => [...DownloadQueryKeyFactory.all, 'client-status'] as const,
	searchJob: (jobId: string) => [...DownloadQueryKeyFactory.all, 'search', jobId] as const,
	tasks: () => [...DownloadQueryKeyFactory.all, 'tasks'] as const,
	// nested under tasks() so the existing invalidateTasks() prefix-invalidates this too
	albumTasks: (mbid: string) => [...DownloadQueryKeyFactory.all, 'tasks', 'album', mbid] as const,
	quarantine: () => [...DownloadQueryKeyFactory.all, 'quarantine'] as const,
	// nested under tasks() so invalidateTasks() prefix-invalidates held lists (all + per-album)
	held: (mbid?: string) =>
		[...DownloadQueryKeyFactory.all, 'tasks', 'held', mbid ?? 'all'] as const,
	indexers: () => [...DownloadQueryKeyFactory.all, 'indexers'] as const,
	sabnzbd: () => [...DownloadQueryKeyFactory.all, 'sabnzbd'] as const,
	policy: () => [...DownloadQueryKeyFactory.all, 'policy'] as const,
	wantedSettings: () => [...DownloadQueryKeyFactory.all, 'wanted-settings'] as const,
	cutoffUnmet: () => [...DownloadQueryKeyFactory.all, 'cutoff-unmet'] as const
};
