import { v3 } from '$lib/api/v3/endpoint';

// v3 Free Music URLs: the task list, history removal and the curator
// actions (cancel, retry). Progress arrives as `free_music_updated` on the
// shared event stream.
const byTask = (taskId: string) => ({ path: { task_id: taskId } });

export const FREE_MUSIC_ENDPOINTS = {
	tasks: (all = false) => v3('/api/v3/free-music/tasks', { query: { all: all || undefined } }),
	clearHistory: (all = false) =>
		v3('/api/v3/free-music/tasks', { query: { all: all || undefined } }),
	remove: (taskId: string) => v3('/api/v3/free-music/tasks/{task_id}', byTask(taskId)),
	cancel: (taskId: string) => v3('/api/v3/free-music/tasks/{task_id}/cancel', byTask(taskId)),
	retry: (taskId: string) => v3('/api/v3/free-music/tasks/{task_id}/retry', byTask(taskId))
} as const;
