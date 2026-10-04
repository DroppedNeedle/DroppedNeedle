import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { clearNavidromeLocalCaches } from '$lib/utils/navidromeLibraryCache';

import { LibraryQueryKeyFactory } from '../library/LibraryQueryKeyFactory';
import { PlaylistQueryKeyFactory } from '../playlists/PlaylistQueryKeyFactory';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { REMOTE_ENDPOINTS } from './endpoints';
import { RemoteQueryKeyFactory } from './RemoteQueryKeyFactory';
import type { RemoteFolderSave, RemoteFolders, RemoteSource } from './types';

export interface ImportRemotePlaylistVars {
	source: RemoteSource;
	id: string;
}

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

function sameUser(contextUserId: string | null | undefined): boolean {
	return !!contextUserId && authStore.user?.id === contextUserId;
}

export const createImportRemotePlaylistMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ImportRemotePlaylistVars) =>
			api.global.v3.POST(REMOTE_ENDPOINTS.importPlaylist(vars.source, vars.id)),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (data, vars, context) => {
			if (!sameUser(context.userId)) return;
			await invalidateQueriesWithPersister({
				queryKey: RemoteQueryKeyFactory.source(context.userId, vars.source)
			});
			// the import lands in local playlists: the playlist list must refresh
			// or the new entry never appears
			await invalidateQueriesWithPersister({
				queryKey: PlaylistQueryKeyFactory.v3.root(context.userId)
			});
			toastStore.show({
				message: `Imported ${data.tracks_imported} track${data.tracks_imported === 1 ? '' : 's'} to your playlists`,
				type: 'success'
			});
		},
		onError: (err) =>
			toastStore.show({
				message: errorMessage(err, 'Could not import that playlist'),
				type: 'error'
			})
	}));

export const createSaveRemoteFoldersMutation = () =>
	createMutation(() => ({
		// Untyped by necessity: the handler takes a FolderSave JSON body, but
		// the generated spec omits the request_body annotation, so the typed
		// client would reject the body. Revisit once the backend annotates it.
		mutationFn: (vars: RemoteFolderSave) =>
			api.global.put<RemoteFolders>(REMOTE_ENDPOINTS.folders(), vars),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (_data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			// a folder-scope change reshapes every navidrome read: drop the
			// folder-scoped local caches, sweep the whole adapter prefix, and
			// bust the unified catalog built on top of it
			clearNavidromeLocalCaches();
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
