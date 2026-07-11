import { createMutation } from '@tanstack/svelte-query';
import {
	createPlaylist,
	generateSmartMix,
	setPlaylistPublic,
	type GenerateSmartMixInput,
	type PlaylistSummary
} from '$lib/api/playlists';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { PlaylistQueryKeyFactory } from './PlaylistQueryKeyFactory';

/** Invalidate the current user's playlist list so a create/delete/share is reflected. */
function invalidateList(): Promise<void> {
	return invalidateQueriesWithPersister({
		queryKey: PlaylistQueryKeyFactory.list(authStore.user?.id)
	});
}

export const createCreatePlaylistMutation = () =>
	createMutation(() => ({
		mutationFn: (name: string) => createPlaylist(name),
		onSuccess: () => invalidateList()
	}));

export const createGenerateSmartMixMutation = () =>
	createMutation(() => ({
		mutationFn: (input: GenerateSmartMixInput) => generateSmartMix(input),
		onSuccess: () => invalidateList()
	}));

export const createSetPlaylistPublicMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: { id: string; isPublic: boolean }) =>
			setPlaylistPublic(vars.id, vars.isPublic),
		onSuccess: async (_data: PlaylistSummary, vars: { id: string; isPublic: boolean }) => {
			await invalidateList();
			await invalidateQueriesWithPersister({
				queryKey: PlaylistQueryKeyFactory.detail(authStore.user?.id, vars.id)
			});
		}
	}));
