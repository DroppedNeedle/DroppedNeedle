import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import { FreeMusicQueryKeyFactory } from './FreeMusicQueryKeyFactory';
import { FREE_MUSIC_ENDPOINTS } from './endpoints';
import type { FreeMusicTasks } from './types';

type Getter<T> = () => T;

const ACTIVE: ReadonlyArray<string> = ['searching', 'downloading', 'importing'];

// Polls while any download is still running so progress lands without a manual
// refresh; the free_music_updated SSE event invalidates on terminal transitions.
export const getFreeMusicTasksQuery = (
	getEnabled: Getter<boolean> = () => true,
	getAll: Getter<boolean> = () => false
) =>
	createQuery(() => ({
		queryKey: FreeMusicQueryKeyFactory.tasks(authStore.user?.id, getAll()),
		queryFn: ({ signal }) =>
			api.global.get<FreeMusicTasks>(FREE_MUSIC_ENDPOINTS.tasks(getAll()), { signal }),
		enabled: getEnabled(),
		refetchInterval: (query: { state: { data?: FreeMusicTasks } }) =>
			query.state.data?.tasks.some((task) => ACTIVE.includes(task.status)) ? 1500 : false
	}));
