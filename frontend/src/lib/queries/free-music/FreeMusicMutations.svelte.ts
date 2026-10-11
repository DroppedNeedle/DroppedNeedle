import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';

import { FreeMusicQueryKeyFactory } from './FreeMusicQueryKeyFactory';
import { FREE_MUSIC_ENDPOINTS } from './endpoints';
import type { FreeMusicTask } from './types';

const invalidateTasks = () =>
	invalidateQueriesWithPersister({ queryKey: FreeMusicQueryKeyFactory.prefix });

export const cancelFreeMusicMutation = () =>
	createMutation(() => ({
		mutationFn: (taskId: string) =>
			api.global.post<FreeMusicTask>(FREE_MUSIC_ENDPOINTS.cancel(taskId)),
		onSuccess: async () => {
			toastStore.show({ message: 'Download cancelled.', type: 'info' });
			await invalidateTasks();
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Cancelling failed.', type: 'error' });
		}
	}));

export const retryFreeMusicMutation = () =>
	createMutation(() => ({
		mutationFn: (taskId: string) =>
			api.global.post<FreeMusicTask>(FREE_MUSIC_ENDPOINTS.retry(taskId)),
		onSuccess: async () => {
			toastStore.show({ message: 'Trying again.', type: 'success' });
			await invalidateTasks();
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Retrying failed.', type: 'error' });
		}
	}));

export const removeFreeMusicHistoryMutation = () =>
	createMutation(() => ({
		mutationFn: (taskId: string) =>
			api.global.delete<{ cleared: number }>(FREE_MUSIC_ENDPOINTS.remove(taskId)),
		onSuccess: async () => {
			toastStore.show({ message: 'Removed from Free Music history.', type: 'info' });
			await invalidateTasks();
		},
		onError: (error: Error) => {
			toastStore.show({
				message: error.message || "Couldn't remove this history entry.",
				type: 'error'
			});
		}
	}));

export const clearFreeMusicHistoryMutation = () =>
	createMutation(() => ({
		mutationFn: (all: boolean) =>
			api.global.delete<{ cleared: number }>(FREE_MUSIC_ENDPOINTS.clearHistory(all)),
		onSuccess: async ({ cleared }) => {
			toastStore.show({
				message:
					cleared === 0
						? 'Nothing to clear.'
						: `Removed ${cleared} ${cleared === 1 ? 'item' : 'items'} from Free Music history.`,
				type: 'info'
			});
			await invalidateTasks();
		},
		onError: (error: Error) => {
			toastStore.show({
				message: error.message || "Couldn't clear Free Music history.",
				type: 'error'
			});
		}
	}));
