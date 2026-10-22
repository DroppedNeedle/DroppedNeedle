import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { invalidateLibraryCatalog } from './LibraryCatalogInvalidation';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';

export type AlbumHistory = components['schemas']['AlbumHistoryView'];
export type AlbumHistoryEntry = components['schemas']['AlbumHistoryEntryView'];

// What the app did by itself to a library album: moves of people's history
// and joins. Under the catalog root, so every catalog refresh reloads it.
export const getAlbumHistoryQuery = (
	userId: Getter<string | undefined>,
	albumId: Getter<string | undefined>
) =>
	createQuery(() => {
		const user = userId();
		const album = albumId();
		return {
			queryKey: LibraryQueryKeyFactory.catalog.albumHistory(user, album ?? ''),
			enabled: Boolean(user && album),
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.global.v3.GET(LibraryV3Api.albumHistory(album ?? ''), { signal })
		};
	});

// Undo the fold of a leftover album, by its history entry's undo_id. The
// album comes back as its own, so every catalog view refreshes; a refusal
// refreshes too, as the history it was offered from is stale.
export function undoAlbumFold() {
	return createMutation(() => ({
		mutationFn: (undoId: string) => api.global.v3.POST(LibraryV3Api.undoAlbumFold(undoId)),
		onSettled: () => invalidateLibraryCatalog()
	}));
}

// Undo the artist an album left on "Unknown Artist" took from its tracks,
// by its history entry's undo_id. The album's artist changes everywhere
// it shows, so every catalog view refreshes, on a refusal too.
export function undoArtistRepair() {
	return createMutation(() => ({
		mutationFn: (undoId: string) => api.global.v3.POST(LibraryV3Api.undoArtistRepair(undoId)),
		onSettled: () => invalidateLibraryCatalog()
	}));
}
