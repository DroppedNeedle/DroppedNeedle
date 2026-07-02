import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import type { AlbumEditionsResponse, EditionAcquireResponse } from '$lib/types';

// Album edition selection (CollectionManagement Feature E). The picker is an
// admin/trusted surface; viewing the list is open to any authenticated user.

const editionsUrl = (mbid: string) => `/api/v1/albums/${encodeURIComponent(mbid)}/editions`;
const pinUrl = (mbid: string) => `/api/v1/albums/${encodeURIComponent(mbid)}/edition`;

export const editionsKey = (mbid: string) => ['albums', 'editions', mbid] as const;

export const getAlbumEditionsQuery = (mbid: Getter<string>, enabled: Getter<boolean>) =>
	createQuery(() => ({
		queryKey: editionsKey(mbid()),
		enabled: enabled() && !!mbid(),
		queryFn: ({ signal }) => api.global.get<AlbumEditionsResponse>(editionsUrl(mbid()), { signal })
	}));

export function setEditionPin() {
	return createMutation(() => ({
		mutationFn: ({ mbid, releaseMbid }: { mbid: string; releaseMbid: string }) =>
			api.global.put(pinUrl(mbid), { release_mbid: releaseMbid }),
		onSuccess: (_d, { mbid }) => invalidateQueriesWithPersister({ queryKey: editionsKey(mbid) })
	}));
}

export function clearEditionPin() {
	return createMutation(() => ({
		mutationFn: ({ mbid }: { mbid: string }) => api.global.delete(pinUrl(mbid)),
		onSuccess: (_d, { mbid }) => invalidateQueriesWithPersister({ queryKey: editionsKey(mbid) })
	}));
}

export function acquireEdition() {
	return createMutation(() => ({
		mutationFn: ({ mbid }: { mbid: string }) =>
			api.global.post<EditionAcquireResponse>(`${pinUrl(mbid)}/acquire`, {}),
		// the acquire fans out into download tasks - surface them in the queue now,
		// not on the next poll
		onSuccess: () => invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.tasks() })
	}));
}
