import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';

import { REMOTE_ENDPOINTS } from './endpoints';
import type { RemoteFolderSave } from './types';
import {
	albumMatchesChanged,
	anyLibraryChanged,
	createAppMutation,
	remoteSourceChanged
} from '$lib/queries/effects';

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

function sameUser(contextUserId: string | null | undefined): boolean {
	return !!contextUserId && authStore.user?.id === contextUserId;
}

export const createSaveRemoteFoldersMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: RemoteFolderSave) => api.v3.PUT(REMOTE_ENDPOINTS.folders(), vars),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: (_data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show({ message: 'Music folder scope saved', type: 'success' });
		},
		// a folder-scope change reshapes every navidrome read: drop the
		// album source matches, sweep the whole adapter prefix, and bust
		// the unified catalog built on top of it
		effects: (_data, _vars, context) =>
			sameUser(context.userId)
				? [
						albumMatchesChanged(context.userId),
						remoteSourceChanged(context.userId, 'navidrome'),
						anyLibraryChanged()
					]
				: [],
		onError: (err) =>
			toastStore.show({
				message: errorMessage(err, 'Could not save the folder scope'),
				type: 'error'
			})
	}));
