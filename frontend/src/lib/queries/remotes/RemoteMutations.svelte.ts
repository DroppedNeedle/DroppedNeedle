import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';

import { AlbumMatchKeyFactory } from '../albumMatch';
import { LibraryQueryKeyFactory } from '../library/LibraryQueryKeyFactory';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { REMOTE_ENDPOINTS } from './endpoints';
import { RemoteQueryKeyFactory } from './RemoteQueryKeyFactory';
import type { RemoteFolderSave } from './types';

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

function sameUser(contextUserId: string | null | undefined): boolean {
	return !!contextUserId && authStore.user?.id === contextUserId;
}

export const createSaveRemoteFoldersMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: RemoteFolderSave) => api.global.v3.PUT(REMOTE_ENDPOINTS.folders(), vars),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (_data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			// a folder-scope change reshapes every navidrome read: drop the
			// album source matches, sweep the whole adapter prefix, and bust
			// the unified catalog built on top of it
			await invalidateQueriesWithPersister({
				queryKey: AlbumMatchKeyFactory.user(context.userId)
			});
			await invalidateQueriesWithPersister({
				queryKey: RemoteQueryKeyFactory.source(context.userId, 'navidrome')
			});
			await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all });
			toastStore.show({ message: 'Music folder scope saved', type: 'success' });
		},
		onError: (err) =>
			toastStore.show({
				message: errorMessage(err, 'Could not save the folder scope'),
				type: 'error'
			})
	}));
