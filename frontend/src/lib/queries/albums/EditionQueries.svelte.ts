import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { musicBrainzSourceKey } from '$lib/queries/musicbrainz/sourceScope.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import type {
	AlbumEditionsResponse,
	EditionAcquireResponse,
	EditionPinResponse
} from '$lib/types';

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

// Per-album edition pins (#382): one MusicBrainz release group can match
// several local albums, so the RG-keyed pin above 409s on those. These
// local-id routes address a single copy and never conflict.
// localAlbumId is a library-local album id (LibraryAlbumSummary.id /
// LibraryAlbumDetail.id) - never an RG MBID. Unowned RGs (no local copies)
// stay on the RG-keyed pin above; only a known local id may enter here.
export const localAlbumEditionPinUrl = (localAlbumId: string) =>
	`/api/v1/library/albums/${encodeURIComponent(localAlbumId)}/edition`;

export const localAlbumEditionPinKey = (
	userId: EditionUserId,
	localAlbumId: string
) => ['albums', 'edition-pin', userId ?? null, localAlbumId] as const;

export const getLocalAlbumEditionPinQuery = (
	getUserId: Getter<EditionUserId>,
	getLocalId: Getter<string>,
	getEnabled: Getter<boolean>
) =>
	createQuery(() => ({
		queryKey: localAlbumEditionPinKey(getUserId(), getLocalId()),
		enabled: getEnabled() && !!getUserId() && !!getLocalId(),
		staleTime: CACHE_TTL.ALBUM_DETAIL_EDITIONS,
		queryFn: ({ signal }) =>
			api.global.get<EditionPinResponse>(localAlbumEditionPinUrl(getLocalId()), { signal })
	}));

type LocalEditionPinVariables = {
	userId: EditionUserId;
	localId: string;
	rgMbid: string;
	releaseMbid: string;
};

type LocalEditionClearVariables = {
	userId: EditionUserId;
	localId: string;
	rgMbid: string;
};

function invalidateLocalPinScope(variables: {
	userId: EditionUserId;
	localId: string;
	rgMbid: string;
}) {
	const invalidations = [
		invalidateQueriesWithPersister({
			queryKey: localAlbumEditionPinKey(variables.userId, variables.localId)
		}),
		invalidateQueriesWithPersister({
			queryKey: editionsKey(variables.userId, variables.rgMbid)
		}),
		invalidateQueriesWithPersister({
			queryKey: LibraryQueryKeyFactory.albumDetail(variables.localId)
		})
	];
	return Promise.all(invalidations);
}

// Guards the per-album boundary: the local id must be known, and it must not
// be the RG MBID itself (an RG id here addresses nothing when unowned and
// resolves ambiguously when owned - both are RG-route work).
function assertLocalAlbumId(localId: string, rgMbid: string): void {
	if (!localId) throw new Error('Missing local album id for the edition pin.');
	if (rgMbid && localId === rgMbid)
		throw new Error('RG MBIDs cannot pin through the per-album edition route.');
}

export function setLocalAlbumEditionPin() {
	return createMutation(() => ({
		mutationFn: ({ localId, rgMbid, releaseMbid }: LocalEditionPinVariables) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.global.put<EditionPinResponse>(localAlbumEditionPinUrl(localId), {
				release_mbid: releaseMbid
			});
		},
		onSuccess: (_d, variables) => invalidateLocalPinScope(variables)
	}));
}

export function clearLocalAlbumEditionPin() {
	return createMutation(() => ({
		mutationFn: ({ localId, rgMbid }: LocalEditionClearVariables) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.global.delete<EditionPinResponse>(localAlbumEditionPinUrl(localId));
		},
		onSuccess: (_d, variables) => invalidateLocalPinScope(variables)
	}));
}
