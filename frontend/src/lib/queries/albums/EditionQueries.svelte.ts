import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { musicBrainzSourceKey } from '$lib/queries/musicbrainz/sourceScope.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import type { AlbumEditionsResponse, EditionAcquireResponse } from '$lib/types';

// CollectionManagement Feature E: the picker is an admin/trusted surface,
// viewing the list is open to any authenticated user.

const editionsUrl = (mbid: string) => `/api/v1/albums/${encodeURIComponent(mbid)}/editions`;
const pinUrl = (mbid: string) => `/api/v1/albums/${encodeURIComponent(mbid)}/edition`;

type EditionUserId = string | null | undefined;

export const editionsKey = (userId: EditionUserId, mbid: string) => {
	const normalizedUserId = userId ?? null;
	return [
		'albums',
		'editions',
		normalizedUserId,
		musicBrainzSourceKey(normalizedUserId),
		mbid
	] as const;
};

export const getAlbumEditionsQuery = (
	getUserId: Getter<EditionUserId>,
	mbid: Getter<string>,
	enabled: Getter<boolean>
) =>
	createQuery(() => ({
		queryKey: editionsKey(getUserId(), mbid()),
		enabled: enabled() && !!getUserId() && !!mbid(),
		staleTime: CACHE_TTL.ALBUM_DETAIL_EDITIONS,
		queryFn: ({ signal }) => api.global.get<AlbumEditionsResponse>(editionsUrl(mbid()), { signal })
	}));

type EditionPinVariables = {
	userId: EditionUserId;
	mbid: string;
	releaseMbid: string;
};

type EditionClearVariables = {
	userId: EditionUserId;
	mbid: string;
};

export function setEditionPin() {
	return createMutation(() => ({
		mutationFn: ({ mbid, releaseMbid }: EditionPinVariables) =>
			api.global.put(pinUrl(mbid), { release_mbid: releaseMbid }),
		onSuccess: (_d, { userId, mbid }) =>
			invalidateQueriesWithPersister({ queryKey: editionsKey(userId, mbid) })
	}));
}

export function clearEditionPin() {
	return createMutation(() => ({
		mutationFn: ({ mbid }: EditionClearVariables) => api.global.delete(pinUrl(mbid)),
		onSuccess: (_d, { userId, mbid }) =>
			invalidateQueriesWithPersister({ queryKey: editionsKey(userId, mbid) })
	}));
}

export function acquireEdition() {
	return createMutation(() => ({
		mutationFn: ({ mbid }: { mbid: string }) =>
			api.global.post<EditionAcquireResponse>(`${pinUrl(mbid)}/acquire`, {}),
		// the acquire fans out into download tasks - surface them in the queue now,
		// not on the next poll
		onSuccess: () =>
			invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.tasks(authStore.user?.id)
			})
	}));
}
