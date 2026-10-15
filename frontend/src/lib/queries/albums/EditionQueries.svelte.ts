import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { purchaseOptionsKey } from '$lib/queries/albums/GetItQueries.svelte';
import { CATALOG_ENDPOINTS } from '$lib/queries/catalog/endpoints';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { invalidateLibraryManagementSurfaces } from '$lib/queries/library-management/LibraryManagementInvalidation';
import { invalidateQueriesWithPersister, queryClient } from '$lib/queries/QueryClient';
import { musicBrainzSourceKey } from '$lib/queries/musicbrainz/sourceScope.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { LibraryV3Api } from '$lib/queries/library/LibraryV3Api';
import { albumBasicCache, albumTracksCache } from '$lib/utils/albumDetailCache';

// Choosing an edition is for curators; the list of a group's editions is
// open to any authenticated user.

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

export function acquireEdition() {
	return createMutation(() => ({
		mutationFn: ({
			mbid,
			copyId,
			replaceVersions
		}: {
			mbid: string;
			copyId?: string;
			replaceVersions?: boolean;
		}) => api.global.v3.POST(CATALOG_ENDPOINTS.acquireEdition(mbid, copyId, replaceVersions)),
		// The acquire starts a download task: show it in the queue, the album's
		// download strip and the album's library status now, not on the next poll.
		onSuccess: (_data, { mbid }) => {
			void invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.tasksPrefix(authStore.user?.id)
			});
			void invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.album(mbid) });
		}
	}));
}

// One library album's edition (library-local album id, never an RG MBID).
// The album identity row is the edition: choosing one goes through the
// library's one edition operation, which accepts any MusicBrainz release.
export type EditionStatus = components['schemas']['EditionStatusView'];
export type EditionChoice = components['schemas']['EditionChoiceView'];
export type WaitingAlbums = components['schemas']['WaitingAlbumsResponse'];

const editionKey = LibraryQueryKeyFactory.catalog.edition;
const editionPlanKey = LibraryQueryKeyFactory.catalog.editionPlan;

// While the files are placed or written, or the album is identified, the
// status is read again this often, so the album page and the edition sheet
// follow along without a reload.
const EDITION_BUSY_POLL_MS = 2_000;

export function editionBusy(status: EditionStatus | null | undefined): boolean {
	return Boolean(status && (status.placing || status.writing || status.identifying));
}

export const getAlbumEditionStatusQuery = (
	getUserId: Getter<EditionUserId>,
	getLocalId: Getter<string>,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => {
		const userId = getUserId();
		const localId = getLocalId();
		const queryKey = editionKey(userId, localId);
		return {
			queryKey,
			enabled: getEnabled() && !!userId && !!localId,
			staleTime: CACHE_TTL.ALBUM_DETAIL_EDITIONS,
			refetchInterval: (query: { state: { data?: EditionStatus } }) =>
				editionBusy(query.state.data) ? EDITION_BUSY_POLL_MS : false,
			queryFn: async ({ signal }: { signal: AbortSignal }): Promise<EditionStatus> => {
				const before = queryClient.getQueryData<EditionStatus>(queryKey);
				const status = await api.global.v3.GET(LibraryV3Api.edition(localId), { signal });
				// The plans line the files up as they were placed: once placing
				// ends, or the edition itself changed, they are stale.
				if (
					before &&
					((before.placing && !status.placing) || before.release_mbid !== status.release_mbid)
				) {
					void invalidateQueriesWithPersister({
						queryKey: LibraryQueryKeyFactory.catalog.editionPlanPrefix(userId, localId)
					});
				}
				return status;
			}
		};
	});

// What taking one edition would do to a library album's files: kept,
// duplicate, other-version or extra files, and the edition's missing tracks.
// A null release plans the album's own edition, which every signed-in user
// may read; another release is for curators.
export type EditionPlan = components['schemas']['EditionPlanView'];

export const getEditionPlanQuery = (
	getUserId: Getter<EditionUserId>,
	getLocalId: Getter<string>,
	getRelease: Getter<string | null>,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		queryKey: editionPlanKey(getUserId(), getLocalId(), getRelease()),
		enabled: getEnabled() && !!getUserId() && !!getLocalId(),
		staleTime: CACHE_TTL.ALBUM_DETAIL_EDITIONS,
		retry: false,
		queryFn: ({ signal }) =>
			api.global.v3.GET(LibraryV3Api.editionPlan(getLocalId(), getRelease() ?? undefined), {
				signal
			})
	}));

// Move the edition's extra files (second copies, files not on it) to the
// recycle bin, and put them back. Never runs on its own.
export function removeEditionExtras() {
	return createMutation(() => ({
		mutationFn: ({ localId, trackIds }: { localId: string; trackIds: string[] }) =>
			api.global.v3.POST(LibraryV3Api.editionExtrasRemove(localId), { track_ids: trackIds }),
		onSuccess: (_data, { localId }) =>
			invalidateEditionScope({ userId: authStore.user?.id, localId })
	}));
}

export function restoreEditionExtras() {
	return createMutation(() => ({
		mutationFn: ({ localId, actionId }: { localId: string; actionId: string }) =>
			api.global.v3.POST(LibraryV3Api.editionExtrasRestore(localId), { action_id: actionId }),
		onSuccess: (_data, { localId }) =>
			invalidateEditionScope({ userId: authStore.user?.id, localId })
	}));
}

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

/**
 * Refresh everything an edition change touches: the album's edition status
 * and plans, its library detail, tracks and copies, the library status the
 * album pages read, and, for the group it left and the one it joined, the
 * album page's cached header and tracklist and the group's editions.
 */
function invalidateEditionScope(
	{ userId, localId, rgMbid }: EditionTarget,
	joinedGroup?: string | null
): void {
	const keys: (readonly unknown[])[] = [
		editionKey(userId, localId),
		LibraryQueryKeyFactory.catalog.albumDetail(userId, localId),
		LibraryQueryKeyFactory.catalog.albumTracksPrefix(userId, localId),
		LibraryQueryKeyFactory.catalog.albumCopiesPrefix(userId),
		LibraryQueryKeyFactory.albumPrefix(),
		[...LibraryQueryKeyFactory.catalog.root(userId), 'unconfirmed']
	];
	for (const group of new Set([rgMbid, joinedGroup])) {
		if (!group) continue;
		albumBasicCache.remove(group);
		albumTracksCache.remove(group);
		keys.push(editionsKey(userId, group), purchaseOptionsKey(group));
	}
	for (const queryKey of keys) void invalidateQueriesWithPersister({ queryKey });
}

export function chooseAlbumEdition() {
	return createMutation(() => ({
		mutationFn: ({ localId, rgMbid, releaseMbid }: EditionTarget & { releaseMbid: string }) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.global.v3.PUT(LibraryV3Api.edition(localId), { release_mbid: releaseMbid });
		},
		onSuccess: (choice, variables) => invalidateEditionScope(variables, choice.release_group_mbid)
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

// An edition change that may set or lift "leave unmanaged" also changes
// what Library Management lists and holds.
function invalidateHeldEditionScope(target: EditionTarget): void {
	invalidateEditionScope(target);
	void invalidateLibraryManagementSurfaces();
}

// Keep the files as they are, as a custom edition of the release's group
// (admins). The revision is the edition status's `identity_revision`.
export function sealCustomEdition() {
	return createMutation(() => ({
		mutationFn: ({
			localId,
			rgMbid,
			releaseMbid,
			identityRevision
		}: EditionTarget & { releaseMbid: string; identityRevision: number }) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.global.v3.POST(LibraryV3Api.editionCustom(localId), {
				release_mbid: releaseMbid,
				expected_identity_revision: identityRevision
			});
		},
		onSuccess: (_d, variables) => invalidateHeldEditionScope(variables)
	}));
}

// Drop the exact edition and keep Library Management away from the album
// (admins); "Manage it again" lifts it.
export function leaveAlbumUnmanaged() {
	return createMutation(() => ({
		mutationFn: ({
			localId,
			rgMbid,
			identityRevision
		}: EditionTarget & { identityRevision: number }) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.global.v3.POST(LibraryV3Api.editionLeaveUnmanaged(localId), {
				expected_identity_revision: identityRevision
			});
		},
		onSuccess: (_d, variables) => invalidateHeldEditionScope(variables)
	}));
}

/**
 * Refresh every screen that shows the album's edition after a change made
 * elsewhere (an identity report's undo): the album page, its edition plate
 * and sheet, and the unconfirmed list.
 */
export function invalidateAlbumEdition(userId: EditionUserId, localId: string): void {
	invalidateEditionScope({ userId, localId });
}

// Writing the chosen edition's tags into the files it placed, in a folder
// the app does not write by itself: a sealed retag preview the person
// reads, then its apply, then, if they want, the undo of that write. The
// write shows in Library Management's history.
export type TagWritePreview = components['schemas']['ManagePreviewResponse'];
export type TagWrite = components['schemas']['ManageApplyResponse'];

export function previewEditionTags() {
	return createMutation(() => ({
		mutationFn: ({
			localId,
			rgMbid,
			files
		}: EditionTarget & { files: EditionChoice['retag_files'] }): Promise<TagWritePreview> => {
			assertLocalAlbumId(localId, rgMbid);
			return api.global.v3.POST(LibraryV3Api.managePreview(), {
				album_id: localId,
				kind: 'retag',
				items: files.map((file) => ({
					root_id: file.root_id,
					rel_path: file.rel_path,
					managed_updates: {}
				}))
			});
		},
		onSuccess: (_d, variables) => invalidateEditionScope(variables)
	}));
}

export function writeEditionTags() {
	return createMutation(() => ({
		mutationFn: ({ previewToken }: EditionTarget & { previewToken: string }): Promise<TagWrite> =>
			api.global.v3.POST(LibraryV3Api.manageApply(), { preview_token: previewToken }),
		onSuccess: (_d, variables) => invalidateHeldEditionScope(variables)
	}));
}

export function undoTagWrite() {
	return createMutation(() => ({
		mutationFn: ({ bundleId }: EditionTarget & { bundleId: string }) =>
			api.global.v3.POST(LibraryV3Api.manageUndo(), { bundle_id: bundleId }),
		onSuccess: (_d, variables) => invalidateHeldEditionScope(variables)
	}));
}

export function undoAlbumEdition() {
	return createMutation(() => ({
		mutationFn: ({ localId }: EditionTarget) =>
			api.global.v3.POST(LibraryV3Api.editionUndo(localId)),
		onSuccess: (_d, variables) => {
			invalidateHeldEditionScope(variables);
			// Identity reports list this album's change too.
			void invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.identityPreparationsPrefix(variables.userId ?? undefined)
			});
			void invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.repairsPrefix() });
		}
	}));
}
