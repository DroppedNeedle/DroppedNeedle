import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
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
