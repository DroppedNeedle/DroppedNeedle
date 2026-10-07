import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { purchaseOptionsKey } from '$lib/queries/albums/GetItQueries.svelte';
import { CATALOG_ENDPOINTS } from '$lib/queries/catalog/endpoints';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { musicBrainzSourceKey } from '$lib/queries/musicbrainz/sourceScope.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { LibraryV3Api } from '$lib/queries/library/LibraryV3Api';
import { albumBasicCache, albumTracksCache } from '$lib/utils/albumDetailCache';

// CollectionManagement Feature E: the picker is an admin/trusted surface,
// viewing the list is open to any authenticated user.

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
		queryFn: ({ signal }) => api.global.v3.GET(CATALOG_ENDPOINTS.editions(mbid()), { signal })
	}));

/**
 * An edition choice changes which release the group serves. This marks the group's
 * edition list and purchase links stale and drops the album page's cached
 * header and tracklist; with a copy id it also refreshes that copy's pin and
 * library album detail. It does not wait for the refetches, and it leaves the
 * library status and the album page reload to the page's own refresh.
 */
function invalidatePinScope(variables: {
	userId: EditionUserId;
	rgMbid: string;
	localId?: string;
}): void {
	const { userId, rgMbid, localId } = variables;
	const keys: (readonly unknown[])[] = [];
	if (rgMbid) {
		albumBasicCache.remove(rgMbid);
		albumTracksCache.remove(rgMbid);
		keys.push(editionsKey(userId, rgMbid), purchaseOptionsKey(rgMbid));
	}
	if (localId) {
		keys.push(
			LibraryQueryKeyFactory.catalog.edition(userId, localId),
			LibraryQueryKeyFactory.catalog.albumDetail(userId, localId)
		);
	}
	for (const queryKey of keys) void invalidateQueriesWithPersister({ queryKey });
}

// The group pin lands on the library's one copy of the group (the server
// answers 404 with none and 409 with several); pass that copy's id when the
// caller knows it, so the copy's own pin and detail refresh too.
type EditionPinVariables = {
	userId: EditionUserId;
	mbid: string;
	releaseMbid: string;
	localId?: string;
};

type EditionClearVariables = {
	userId: EditionUserId;
	mbid: string;
	localId?: string;
};

export function setEditionPin() {
	return createMutation(() => ({
		mutationFn: ({ mbid, releaseMbid }: EditionPinVariables) =>
			api.global.v3.PUT(CATALOG_ENDPOINTS.editionPin(mbid), { release_mbid: releaseMbid }),
		onSuccess: (_d, { userId, mbid, localId }) =>
			invalidatePinScope({ userId, rgMbid: mbid, localId })
	}));
}

export function clearEditionPin() {
	return createMutation(() => ({
		mutationFn: ({ mbid }: EditionClearVariables) =>
			api.global.v3.DELETE(CATALOG_ENDPOINTS.editionPin(mbid)),
		onSuccess: (_d, { userId, mbid, localId }) =>
			invalidatePinScope({ userId, rgMbid: mbid, localId })
	}));
}

export function acquireEdition() {
	return createMutation(() => ({
		mutationFn: ({ mbid }: { mbid: string }) =>
			api.global.v3.POST(CATALOG_ENDPOINTS.acquireEdition(mbid)),
		// the acquire fans out into download tasks - surface them in the queue now,
		// not on the next poll
		onSuccess: () =>
			invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.tasks(authStore.user?.id)
			})
	}));
}

// One library album's edition (library-local album id, never an RG MBID).
// The album identity row is the edition: choosing one goes through the
// library's one edition operation, which accepts any MusicBrainz release.
export type EditionStatus = components['schemas']['EditionStatusView'];
export type EditionChoice = components['schemas']['EditionChoiceView'];
export type WaitingAlbums = components['schemas']['WaitingAlbumsResponse'];

const editionKey = LibraryQueryKeyFactory.catalog.edition;

export const getAlbumEditionStatusQuery = (
	getUserId: Getter<EditionUserId>,
	getLocalId: Getter<string>,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		queryKey: editionKey(getUserId(), getLocalId()),
		enabled: getEnabled() && !!getUserId() && !!getLocalId(),
		staleTime: CACHE_TTL.ALBUM_DETAIL_EDITIONS,
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.edition(getLocalId()), { signal })
	}));

export const getEditionTracksQuery = (
	getGroup: Getter<string>,
	getRelease: Getter<string | null>
) =>
	createQuery(() => ({
		queryKey: ['albums', 'edition-tracks', getRelease()] as const,
		enabled: !!getGroup() && !!getRelease(),
		staleTime: CACHE_TTL.ALBUM_DETAIL_EDITIONS,
		queryFn: ({ signal }) =>
			api.global.v3.GET(CATALOG_ENDPOINTS.editionTracks(getGroup(), getRelease() ?? ''), {
				signal
			})
	}));

export const getWaitingAlbumsQuery = (
	getUserId: Getter<EditionUserId>,
	getState: Getter<'unconfirmed' | 'unmatched'>,
	getOffset: Getter<number>,
	limit = 50
) =>
	createQuery(() => ({
		queryKey: LibraryQueryKeyFactory.catalog.unconfirmed(getUserId(), getState(), getOffset()),
		enabled: !!getUserId(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LibraryV3Api.unconfirmed(getState(), limit, getOffset()), { signal })
	}));

type EditionTarget = { userId: EditionUserId; localId: string; rgMbid?: string };

// Guards the per-album boundary: the local id must be known, and it must not
// be the RG MBID itself.
function assertLocalAlbumId(localId: string, rgMbid?: string): void {
	if (!localId) throw new Error('Missing local album id for the edition.');
	if (rgMbid && localId === rgMbid)
		throw new Error('Choose the edition on the library copy, not the release group.');
}

/** Refresh everything an edition change touches. */
function invalidateEditionScope({ userId, localId, rgMbid }: EditionTarget): void {
	invalidatePinScope({ userId, rgMbid: rgMbid ?? '', localId });
	void invalidateQueriesWithPersister({ queryKey: editionKey(userId, localId) });
	void invalidateQueriesWithPersister({
		queryKey: [...LibraryQueryKeyFactory.catalog.root(userId), 'unconfirmed']
	});
}

export function chooseAlbumEdition() {
	return createMutation(() => ({
		mutationFn: ({ localId, rgMbid, releaseMbid }: EditionTarget & { releaseMbid: string }) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.global.v3.PUT(LibraryV3Api.edition(localId), { release_mbid: releaseMbid });
		},
		onSuccess: (_d, variables) => invalidateEditionScope(variables)
	}));
}

export function handBackAlbumEdition() {
	return createMutation(() => ({
		mutationFn: ({ localId, rgMbid }: EditionTarget) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.global.v3.DELETE(LibraryV3Api.edition(localId));
		},
		onSuccess: (_d, variables) => invalidateEditionScope(variables)
	}));
}

export function confirmAlbumEdition() {
	return createMutation(() => ({
		mutationFn: ({ localId }: EditionTarget) =>
			api.global.v3.POST(LibraryV3Api.editionConfirm(localId)),
		onSuccess: (_d, variables) => invalidateEditionScope(variables)
	}));
}

export function undoAlbumEdition() {
	return createMutation(() => ({
		mutationFn: ({ localId }: EditionTarget) =>
			api.global.v3.POST(LibraryV3Api.editionUndo(localId)),
		onSuccess: (_d, variables) => invalidateEditionScope(variables)
	}));
}

/**
 * Write the chosen edition's tags into the files it placed: a sealed retag
 * preview, applied at once (the person already asked for it).
 */
export function retagAfterChoice() {
	return createMutation(() => ({
		mutationFn: async ({
			localId,
			files
		}: EditionTarget & { files: EditionChoice['retag_files'] }) => {
			const preview = await api.global.v3.POST(LibraryV3Api.managePreview(), {
				album_id: localId,
				kind: 'retag',
				items: files.map((file) => ({
					root_id: file.root_id,
					rel_path: file.rel_path,
					managed_updates: {}
				}))
			});
			return api.global.v3.POST(LibraryV3Api.manageApply(), {
				preview_token: preview.preview_token
			});
		},
		onSuccess: (_d, variables) => invalidateEditionScope(variables)
	}));
}
