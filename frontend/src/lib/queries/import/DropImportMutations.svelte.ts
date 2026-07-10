import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { LOCAL_KEYS } from '$lib/queries/local/LocalQueries.svelte';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { toastStore } from '$lib/stores/toast';

import { DropImportQueryKeyFactory } from './DropImportQueryKeyFactory';
import type { DropImportItem, DropImportJob } from './types';

const invalidateJobs = () =>
	invalidateQueriesWithPersister({ queryKey: DropImportQueryKeyFactory.prefix });

// An import lands albums in the library: sweep the library, local-player, and
// home caches too (cross-domain rule), or a reload shows stale grids.
const invalidateLibrarySurfaces = async () => {
	await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all });
	await invalidateQueriesWithPersister({ queryKey: LOCAL_KEYS.root });
	await invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix });
};

export const uploadDropMutation = () =>
	createMutation(() => ({
		mutationFn: (files: File[]) => {
			const form = new FormData();
			for (const file of files) form.append('files', file, file.name);
			return api.global.upload<DropImportJob>(API.dropImport.uploads(), form);
		},
		onSuccess: async () => {
			toastStore.show({ message: 'Import started - identifying your files.', type: 'success' });
			await invalidateJobs();
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Upload failed.', type: 'error' });
		}
	}));

export const matchDropItemMutation = () =>
	createMutation(() => ({
		mutationFn: ({ itemId, releaseGroupMbid }: { itemId: number; releaseGroupMbid: string }) =>
			api.global.post<DropImportItem>(API.dropImport.match(itemId), {
				release_group_mbid: releaseGroupMbid
			}),
		onSuccess: async (item) => {
			toastStore.show({
				message:
					item.status === 'imported'
						? `Imported ${item.album_title ?? 'the album'}.`
						: `Nothing new to import: ${item.detail ?? 'already in your library'}.`,
				type: item.status === 'imported' ? 'success' : 'info'
			});
			await invalidateJobs();
			await invalidateLibrarySurfaces();
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Match failed.', type: 'error' });
		}
	}));

export const discardDropItemMutation = () =>
	createMutation(() => ({
		mutationFn: (itemId: number) => api.global.post<DropImportItem>(API.dropImport.discard(itemId)),
		onSuccess: async () => {
			toastStore.show({ message: 'Discarded.', type: 'info' });
			await invalidateJobs();
		},
		onError: (error: Error) => {
			toastStore.show({ message: error.message || 'Discard failed.', type: 'error' });
		}
	}));
