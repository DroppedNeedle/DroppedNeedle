import { createMutation, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { purchaseOptionsKey } from '$lib/queries/albums/GetItQueries.svelte';
import { CATALOG_ENDPOINTS } from '$lib/queries/catalog/endpoints';
import { REQUESTS_ENDPOINTS } from '$lib/queries/requests/endpoints';
import { RequestQueryKeyFactory } from '$lib/queries/requests/RequestQueryKeyFactory';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { invalidateLibraryManagementSurfaces } from '$lib/queries/library-management/LibraryManagementInvalidation';
import {
	FavoriteQueryKeyFactory,
	PlaylistQueryKeyFactory
} from '$lib/queries/playlists/PlaylistQueryKeyFactory';
import { RatingQueryKeyFactory } from '$lib/queries/ratings/RatingQueryKeyFactory';
import { invalidateQueriesWithPersister, queryClient } from '$lib/queries/QueryClient';
import { musicBrainzSourceKey } from '$lib/queries/musicbrainz/sourceScope.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { LibraryV3Api } from '$lib/queries/library/LibraryV3Api';
import { AlbumQueryKeyFactory } from '$lib/queries/catalog/AlbumQueryKeyFactory';

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

export const editionTracksKey = (userId: EditionUserId, release: string) => {
	const normalizedUserId = userId ?? null;
	return [
		'albums',
		'edition-tracks',
		normalizedUserId,
		musicBrainzSourceKey(normalizedUserId),
		release
	] as const;
};

// One edition's tracklist, for an album the library does not hold.
export const getEditionTracksQuery = (
	getUserId: Getter<EditionUserId>,
	group: Getter<string>,
	release: Getter<string | null>,
	enabled: Getter<boolean>
) =>
	createQuery(() => ({
		queryKey: editionTracksKey(getUserId(), release() ?? ''),
		enabled: enabled() && !!getUserId() && !!group() && !!release(),
		staleTime: CACHE_TTL.ALBUM_DETAIL_EDITIONS,
		queryFn: ({ signal }) =>
			api.global.v3.GET(CATALOG_ENDPOINTS.editionTracks(group(), release() ?? ''), { signal })
	}));

// Switch a live request to another edition (curators). The answer says
// whether it switched or the download had already started.
export function switchRequestedEdition() {
	return createMutation(() => ({
		mutationFn: (input: {
			group: string;
			release: string;
			artistName: string | null;
			albumTitle: string;
		}) =>
			api.global.v3.POST(REQUESTS_ENDPOINTS.requestAlbum(), {
				musicbrainz_id: input.group,
				artist: input.artistName,
				album: input.albumTitle,
				release_mbid: input.release
			}),
		onSettled: () => {
			void invalidateQueriesWithPersister({ queryKey: RequestQueryKeyFactory.all });
		}
	}));
}

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
		// download strip, its edition fetch and its library status now, not on
		// the next poll.
		onSuccess: (_data, { mbid }) => {
			void invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.tasksPrefix(authStore.user?.id)
			});
			void invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.album(mbid) });
		}
	}));
}

// The album's current or last edition fetch, and where each track it asked
// for is (curators). Kept under the user's download tasks, so starting,
// cancelling or retrying any download reads it again.
export type EditionFetch = components['schemas']['EditionFetchView'];
export type EditionFetchTrackState = components['schemas']['EditionFetchTrackState'];

export const editionFetchKey = (userId: EditionUserId, group: string) =>
	[...DownloadQueryKeyFactory.tasks(userId ?? undefined), 'edition', group] as const;

const EDITION_FETCH_POLL_MS = 2_500;

export type EditionFetchAnswer = components['schemas']['EditionFetchResponse'];

export function editionFetchLive(edition: EditionFetch | null | undefined): boolean {
	return edition?.state === 'waiting' || edition?.state === 'fetching';
}

/** Tracks the fetch has landed so far. */
function landedCount(edition: EditionFetch | null | undefined): number {
	return edition?.tracks.filter((track) => track.state === 'landed').length ?? 0;
}

export const getEditionFetchQuery = (
	getUserId: Getter<EditionUserId>,
	getGroup: Getter<string>,
	getLocalId: Getter<string>,
	getEnabled: Getter<boolean> = () => true
) =>
	createQuery(() => {
		const userId = getUserId();
		const group = getGroup();
		const localId = getLocalId();
		const queryKey = editionFetchKey(userId, group);
		return {
			queryKey,
			enabled: getEnabled() && !!userId && !!group,
			staleTime: 0,
			refetchInterval: (query: { state: { data?: EditionFetchAnswer } }) =>
				editionFetchLive(query.state.data?.latest) ? EDITION_FETCH_POLL_MS : false,
			queryFn: async ({ signal }: { signal: AbortSignal }): Promise<EditionFetchAnswer> => {
				const before = queryClient.getQueryData<EditionFetchAnswer>(queryKey)?.latest;
				const answer = await api.global.v3.GET(
					v3('/api/v3/albums/{album_id}/edition/acquire', { path: { album_id: group } }),
					{ signal }
				);
				// A track that landed, or a fetch that ended, changes the album's
				// files: its edition plan and track rows are stale.
				const after = answer.latest;
				if (
					localId &&
					before &&
					(landedCount(after) > landedCount(before) ||
						(editionFetchLive(before) && !editionFetchLive(after)))
				) {
					invalidateEditionScope({ userId, localId, rgMbid: group });
				}
				return answer;
			}
		};
	});

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
				// The plans line the files up as they were placed, and say which
				// hand matches were written since: once placing or writing ends,
				// or the edition itself changed, they are stale.
				if (
					before &&
					((before.placing && !status.placing) ||
						(before.writing && !status.writing) ||
						before.release_mbid !== status.release_mbid)
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
// recycle bin. Never runs on its own. The album's removals list it for
// Put back.
export function removeEditionExtras() {
	return createMutation(() => ({
		mutationFn: ({ localId, trackIds }: { localId: string; trackIds: string[] }) =>
			api.global.v3.POST(LibraryV3Api.editionExtrasRemove(localId), { track_ids: trackIds }),
		onSuccess: (_data, { localId }) => {
			const userId = authStore.user?.id;
			invalidateEditionScope({ userId, localId });
			void invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.catalog.albumRemovalsPrefix(userId)
			});
		}
	}));
}

// Match one file to a track of the album's edition by hand, and undo it.
// A match can carry a missing track's plays, stars, ratings and playlist entries
// to the file, for every listener, so those lists refresh too. A refusal
// means the plan on screen was stale, so it is read again either way.
function invalidateHandMatchScope(localId: string): void {
	const userId = authStore.user?.id;
	invalidateEditionScope({ userId, localId });
	for (const queryKey of [
		LibraryQueryKeyFactory.catalog.albumHistory(userId, localId),
		FavoriteQueryKeyFactory.prefix,
		RatingQueryKeyFactory.prefix,
		PlaylistQueryKeyFactory.prefix
	]) {
		void invalidateQueriesWithPersister({ queryKey });
	}
}

export function matchEditionFile() {
	return createMutation(() => ({
		mutationFn: ({
			localId,
			trackId,
			releaseTrackMbid,
			identityRevision
		}: {
			localId: string;
			trackId: string;
			releaseTrackMbid: string;
			identityRevision: number;
		}) =>
			api.global.v3.PUT(LibraryV3Api.editionFile(localId, trackId), {
				release_track_mbid: releaseTrackMbid,
				expected_identity_revision: identityRevision
			}),
		onSettled: (_data, _error, { localId }) => invalidateHandMatchScope(localId)
	}));
}

export function unmatchEditionFile() {
	return createMutation(() => ({
		mutationFn: ({ localId, trackId }: { localId: string; trackId: string }) =>
			api.global.v3.DELETE(LibraryV3Api.editionFile(localId, trackId)),
		onSettled: (_data, _error, { localId }) => invalidateHandMatchScope(localId)
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
		keys.push(
			AlbumQueryKeyFactory.basic(userId, group),
			AlbumQueryKeyFactory.tracks(userId, group),
			editionsKey(userId, group),
			purchaseOptionsKey(group)
		);
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

// Writing the chosen edition's tags into its files, in a folder the app
// does not write by itself, as the folder's Library Management profile
// writes them: a preview job the person reads once it is planned, then its
// apply, then, if they want, the undo of that write, planned and applied
// the same way. The write shows in Library Management's history.
export type TagWriteQueued = components['schemas']['EditionTagsQueuedView'];
export type TagWriteJob = components['schemas']['EditionTagsJobView'];

// A tag write still planning or writing is read again this often.
const TAG_WRITE_POLL_MS = 1_000;

function tagWriteMoving(job: TagWriteJob | null | undefined): boolean {
	return !job || job.step === 'waiting' || job.step === 'planning' || job.step === 'writing';
}

export const getTagWriteQuery = (
	getUserId: Getter<EditionUserId>,
	getLocalId: Getter<string>,
	getJobId: Getter<string | null>
) =>
	createQuery(() => {
		const userId = getUserId();
		const localId = getLocalId();
		const jobId = getJobId() ?? '';
		const queryKey = LibraryQueryKeyFactory.catalog.editionTagWrite(userId, localId, jobId);
		return {
			queryKey,
			enabled: !!userId && !!localId && !!jobId,
			staleTime: 0,
			retry: false,
			refetchInterval: (query: { state: { data?: TagWriteJob } }) =>
				tagWriteMoving(query.state.data) ? TAG_WRITE_POLL_MS : false,
			queryFn: ({ signal }: { signal: AbortSignal }): Promise<TagWriteJob> =>
				api.global.v3.GET(LibraryV3Api.editionTagWrite(localId, jobId), { signal })
		};
	});

/** A tag write or its undo finished: everything that shows the files is stale. */
export function tagWriteFinished(target: EditionTarget): void {
	invalidateHeldEditionScope(target);
}

export function previewEditionTags() {
	return createMutation(() => ({
		mutationFn: ({
			localId,
			rgMbid,
			releaseMbid,
			identityRevision
		}: EditionTarget & {
			releaseMbid: string;
			identityRevision: number;
		}): Promise<TagWriteQueued> => {
			assertLocalAlbumId(localId, rgMbid);
			return api.global.v3.POST(LibraryV3Api.editionTags(localId), {
				release_mbid: releaseMbid,
				expected_identity_revision: identityRevision
			});
		}
	}));
}

export function writeEditionTags() {
	return createMutation(() => ({
		mutationFn: ({
			localId,
			jobId,
			previewToken,
			rowRevision
		}: EditionTarget & {
			jobId: string;
			previewToken: string;
			rowRevision: number;
		}): Promise<TagWriteJob> =>
			api.global.v3.POST(LibraryV3Api.editionTagWriteApply(localId, jobId), {
				preview_token: previewToken,
				expected_row_revision: rowRevision,
				idempotency_key: `apply-${jobId}`
			}),
		onSuccess: (_job, variables) => invalidateHeldEditionScope(variables)
	}));
}

export function undoTagWrite() {
	return createMutation(() => ({
		mutationFn: ({
			localId,
			jobId,
			rowRevision
		}: EditionTarget & { jobId: string; rowRevision: number }): Promise<TagWriteQueued> =>
			api.global.v3.POST(LibraryV3Api.editionTagWriteUndo(localId, jobId), {
				expected_row_revision: rowRevision,
				idempotency_key: `undo-${jobId}`
			}),
		onSuccess: (_queued, variables) => invalidateHeldEditionScope(variables)
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
