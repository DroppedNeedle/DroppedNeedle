import {
	albumGroupChanged,
	albumHistoryChanged,
	albumRemovalsChanged,
	albumStatusChanged,
	anyRequestChanged,
	applyEffects,
	catalogChanged,
	createAppMutation,
	downloadsChanged,
	editionChanged,
	editionPlansChanged,
	type Effect,
	favoritesChanged,
	identityPreparationsChanged,
	playlistsChanged,
	ratingsChanged,
	repairsChanged
} from '$lib/queries/effects';
import { createInfiniteQuery, createQuery, skipToken } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { STALE_LIVE, STALE_SLOW } from '$lib/queries/staleTime';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { queryClient } from '$lib/queries/QueryClient';
import { PROVIDER } from '$lib/queries/persistedCache';
import { authStore } from '$lib/stores/authStore.svelte';
import { AlbumQueryKeyFactory } from '$lib/queries/catalog/AlbumQueryKeyFactory';

// Choosing an edition is for curators; the list of a group's editions is
// open to any authenticated user.

type EditionUserId = string | null | undefined;

export const getAlbumEditionsQuery = (
	getUserId: Getter<EditionUserId>,
	mbid: Getter<string>,
	enabled: Getter<boolean>
) =>
	createQuery(() => ({
		queryKey: AlbumQueryKeyFactory.editions(getUserId(), mbid()),
		meta: PROVIDER,
		enabled: enabled() && !!getUserId() && !!mbid(),
		staleTime: STALE_SLOW,
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/albums/{album_id}/editions', { path: { album_id: mbid() } }), {
				signal
			})
	}));

// One edition's tracklist, for an album the library does not hold.
export const getEditionTracksQuery = (
	getUserId: Getter<EditionUserId>,
	group: Getter<string>,
	release: Getter<string | null>,
	enabled: Getter<boolean>
) =>
	createQuery(() => ({
		queryKey: AlbumQueryKeyFactory.editionTracks(getUserId(), release() ?? ''),
		meta: PROVIDER,
		enabled: enabled() && !!getUserId() && !!group() && !!release(),
		staleTime: STALE_SLOW,
		queryFn: ({ signal }) =>
			api.v3.GET(
				v3('/api/v3/albums/{album_id}/editions/{release_mbid}/tracks', {
					path: { album_id: group(), release_mbid: release() ?? '' }
				}),
				{ signal }
			)
	}));

// Switch a live request to another edition (curators). The answer says
// whether it switched or the download had already started.
export function switchRequestedEdition() {
	return createAppMutation(() => ({
		mutationFn: (input: {
			group: string;
			release: string;
			artistName: string | null;
			albumTitle: string;
		}) =>
			api.v3.POST(v3('/api/v3/requests/albums'), {
				musicbrainz_id: input.group,
				artist: input.artistName,
				album: input.albumTitle,
				release_mbid: input.release
			}),
		effects: () => [anyRequestChanged()],
		onError: () => void applyEffects(anyRequestChanged())
	}));
}

// `copyId` names the library copy the edition picker planned;
// `replaceVersions` (the server's default is on) fetches the edition's
// version of each track the copy holds in another version, to replace it.
export function acquireEdition() {
	return createAppMutation(() => ({
		mutationFn: ({
			mbid,
			copyId,
			replaceVersions
		}: {
			mbid: string;
			copyId?: string;
			replaceVersions?: boolean;
		}) =>
			api.v3.POST(
				v3('/api/v3/albums/{album_id}/edition/acquire', {
					path: { album_id: mbid },
					query: {
						...(copyId ? { album_id: copyId } : {}),
						...(replaceVersions === undefined ? {} : { replace_versions: replaceVersions })
					}
				})
			),
		// The acquire starts a download task: show it in the queue, the album's
		// download strip, its edition fetch and its library status now, not on
		// the next poll.
		effects: (_data, { mbid }) => [
			downloadsChanged(authStore.user?.id),
			albumStatusChanged(authStore.user?.id, mbid)
		]
	}));
}

// The album's current or last edition fetch, and where each track it asked
// for is (curators). Kept under the user's download tasks, so starting,
// cancelling or retrying any download reads it again.
export type EditionFetch = components['schemas']['EditionFetchView'];
export type EditionFetchTrackState = components['schemas']['EditionFetchTrackState'];

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
		const queryKey = DownloadQueryKeyFactory.editionFetch(userId, group);
		return {
			queryKey,
			enabled: getEnabled() && !!userId && !!group,
			staleTime: STALE_LIVE,
			refetchInterval: (query: { state: { data?: EditionFetchAnswer } }) =>
				editionFetchLive(query.state.data?.latest) ? EDITION_FETCH_POLL_MS : false,
			queryFn: async ({ signal }: { signal: AbortSignal }): Promise<EditionFetchAnswer> => {
				const before = queryClient.getQueryData<EditionFetchAnswer>(queryKey)?.latest;
				const answer = await api.v3.GET(
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
					void applyEffects(...editionScope({ userId, localId, rgMbid: group }));
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
export type WaitingAlbums = components['schemas']['Page_WaitingAlbumView'];

const editionPlanKey = LibraryQueryKeyFactory.catalog.editionPlan;

// While the files are placed or written, or the album is identified, the
// status is read again this often, so the album page and the edition sheet
// follow along without a reload.
const EDITION_BUSY_POLL_MS = 2_000;

function editionBusy(status: EditionStatus | null | undefined): boolean {
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
		const queryKey = LibraryQueryKeyFactory.catalog.edition(userId, localId);
		return {
			queryKey,
			enabled: getEnabled() && !!userId && !!localId,
			staleTime: STALE_SLOW,
			refetchInterval: (query: { state: { data?: EditionStatus } }) =>
				editionBusy(query.state.data) ? EDITION_BUSY_POLL_MS : false,
			queryFn: async ({ signal }: { signal: AbortSignal }): Promise<EditionStatus> => {
				const before = queryClient.getQueryData<EditionStatus>(queryKey);
				const status = await api.v3.GET(
					v3('/api/v3/library/albums/{album_id}/edition', { path: { album_id: localId } }),
					{ signal }
				);
				// The plans line the files up as they were placed, and say which
				// hand matches were written since: once placing or writing ends,
				// or the edition itself changed, they are stale.
				if (
					before &&
					((before.placing && !status.placing) ||
						(before.writing && !status.writing) ||
						before.release_mbid !== status.release_mbid)
				) {
					void applyEffects(editionPlansChanged(userId, localId));
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
		staleTime: STALE_SLOW,
		retry: false,
		queryFn: ({ signal }) => {
			const release = getRelease();
			return api.v3.GET(
				v3('/api/v3/library/albums/{album_id}/edition/plan', {
					path: { album_id: getLocalId() },
					query: release ? { release_mbid: release } : {}
				}),
				{ signal }
			);
		}
	}));

// Move the edition's extra files (second copies, files not on it) to the
// recycle bin. Never runs on its own. The album's removals list it for
// Put back.
export function removeEditionExtras() {
	return createAppMutation(() => ({
		mutationFn: ({
			localId,
			trackIds,
			identityRevision
		}: {
			localId: string;
			trackIds: string[];
			identityRevision: number;
		}) =>
			api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/edition/extras/remove', {
					path: { album_id: localId }
				}),
				{
					track_ids: trackIds,
					expected_identity_revision: identityRevision
				}
			),
		effects: (_data, { localId }) => {
			const userId = authStore.user?.id;
			return [...editionScope({ userId, localId }), albumRemovalsChanged(userId)];
		}
	}));
}

// Match one file to a track of the album's edition by hand, and undo it.
// A match can carry a missing track's plays, stars, ratings and playlist entries
// to the file, for every listener, so those lists refresh too. A refusal
// means the plan on screen was stale, so it is read again either way.
function handMatchScope(localId: string): Effect[] {
	const userId = authStore.user?.id;
	return [
		...editionScope({ userId, localId }),
		albumHistoryChanged(userId, localId),
		favoritesChanged(userId),
		ratingsChanged(userId),
		playlistsChanged(userId)
	];
}

export function matchEditionFile() {
	return createAppMutation(() => ({
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
			api.v3.PUT(
				v3('/api/v3/library/albums/{album_id}/edition/files/{track_id}', {
					path: { album_id: localId, track_id: trackId }
				}),
				{
					release_track_mbid: releaseTrackMbid,
					expected_identity_revision: identityRevision
				}
			),
		effects: (_data, { localId }) => handMatchScope(localId),
		onError: (_error, { localId }) => applyEffects(...handMatchScope(localId))
	}));
}

export function unmatchEditionFile() {
	return createAppMutation(() => ({
		mutationFn: ({
			localId,
			trackId,
			identityRevision
		}: {
			localId: string;
			trackId: string;
			identityRevision: number;
		}) =>
			api.v3.DELETE(
				v3('/api/v3/library/albums/{album_id}/edition/files/{track_id}', {
					path: { album_id: localId, track_id: trackId },
					query: { expected_identity_revision: identityRevision }
				})
			),
		effects: (_data, { localId }) => handMatchScope(localId),
		onError: (_error, { localId }) => applyEffects(...handMatchScope(localId))
	}));
}

export const getWaitingAlbumsQuery = (
	getUserId: Getter<EditionUserId>,
	getState: Getter<'unconfirmed' | 'unmatched'>,
	limit = 50
) =>
	createInfiniteQuery(() => {
		const state = getState();
		return {
			queryKey: LibraryQueryKeyFactory.catalog.unconfirmed(getUserId(), state),
			enabled: !!getUserId(),
			initialPageParam: undefined as string | undefined,
			queryFn: ({ pageParam, signal }): Promise<WaitingAlbums> =>
				api.v3.GET(
					v3('/api/v3/library/unconfirmed', {
						query: { state, limit, ...(pageParam ? { cursor: pageParam } : {}) }
					}),
					{ signal }
				),
			getNextPageParam: (lastPage: WaitingAlbums) => lastPage.next_cursor ?? undefined
		};
	});

type EditionTarget = { userId: EditionUserId; localId: string; rgMbid?: string };
// The edition status's `identity_revision` the action was decided on.
type GuardedEditionTarget = EditionTarget & { identityRevision: number };

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
function editionScope(
	{ userId, localId, rgMbid }: EditionTarget,
	joinedGroup?: string | null
): Effect[] {
	const effects = [editionChanged(userId, localId)];
	for (const group of new Set([rgMbid, joinedGroup])) {
		if (group) effects.push(albumGroupChanged(userId, group));
	}
	return effects;
}

export function chooseAlbumEdition() {
	return createAppMutation(() => ({
		mutationFn: ({
			localId,
			rgMbid,
			releaseMbid,
			identityRevision
		}: GuardedEditionTarget & { releaseMbid: string }) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.v3.PUT(
				v3('/api/v3/library/albums/{album_id}/edition', { path: { album_id: localId } }),
				{
					release_mbid: releaseMbid,
					expected_identity_revision: identityRevision
				}
			);
		},
		effects: (choice, variables) => editionScope(variables, choice.release_group_mbid)
	}));
}

export function handBackAlbumEdition() {
	return createAppMutation(() => ({
		mutationFn: ({ localId, rgMbid, identityRevision }: GuardedEditionTarget) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.v3.DELETE(
				v3('/api/v3/library/albums/{album_id}/edition', {
					path: { album_id: localId },
					query: { expected_identity_revision: identityRevision }
				})
			);
		},
		effects: (_d, variables) => editionScope(variables)
	}));
}

export function confirmAlbumEdition() {
	return createAppMutation(() => ({
		mutationFn: ({ localId, identityRevision }: GuardedEditionTarget) =>
			api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/edition/confirm', { path: { album_id: localId } }),
				{
					expected_identity_revision: identityRevision
				}
			),
		effects: (_d, variables) => editionScope(variables)
	}));
}

// An edition change that may set or lift "leave unmanaged" also changes
// what Library Management lists and holds.
function heldEditionScope(target: EditionTarget): Effect[] {
	return [...editionScope(target), catalogChanged(authStore.user?.id)];
}

// Keep the files as they are, as a custom edition of the release's group
// (admins). The revision is the edition status's `identity_revision`.
export function sealCustomEdition() {
	return createAppMutation(() => ({
		mutationFn: ({
			localId,
			rgMbid,
			releaseMbid,
			identityRevision
		}: EditionTarget & { releaseMbid: string; identityRevision: number }) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/edition/custom', { path: { album_id: localId } }),
				{
					release_mbid: releaseMbid,
					expected_identity_revision: identityRevision
				}
			);
		},
		effects: (_d, variables) => heldEditionScope(variables)
	}));
}

// Drop the exact edition and keep Library Management away from the album
// (admins); "Manage it again" lifts it.
export function leaveAlbumUnmanaged() {
	return createAppMutation(() => ({
		mutationFn: ({
			localId,
			rgMbid,
			identityRevision
		}: EditionTarget & { identityRevision: number }) => {
			assertLocalAlbumId(localId, rgMbid);
			return api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/edition/leave-unmanaged', {
					path: { album_id: localId }
				}),
				{
					expected_identity_revision: identityRevision
				}
			);
		},
		effects: (_d, variables) => heldEditionScope(variables)
	}));
}

/**
 * Refresh every screen that shows the album's edition after a change made
 * elsewhere (an identity report's undo): the album page, its edition plate
 * and sheet, and the unconfirmed list.
 */
export function albumEditionChanged(userId: EditionUserId, localId: string): Effect[] {
	return editionScope({ userId, localId });
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
const TAG_WRITE_WATCH_SLOWDOWN = 1.5;
const TAG_WRITE_WATCH_MAX_POLL_MS = 30_000;
const TAG_WRITE_WATCH_LIMIT_MS = 60 * 60 * 1_000;

function tagWriteMoving(job: TagWriteJob | null | undefined): boolean {
	return !job || job.step === 'waiting' || job.step === 'planning' || job.step === 'writing';
}

// A 4xx is the server's answer about the job; 429 and anything else may pass.
function tagWriteUnreadable(error: unknown): boolean {
	return (
		error instanceof ApiError && error.status >= 400 && error.status < 500 && error.status !== 429
	);
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
			staleTime: STALE_LIVE,
			retry: false,
			refetchInterval: (query: { state: { data?: TagWriteJob } }) =>
				tagWriteMoving(query.state.data) ? TAG_WRITE_POLL_MS : false,
			// No job, no read: a refresh after a write must not fetch an empty id.
			queryFn: jobId
				? ({ signal }: { signal: AbortSignal }): Promise<TagWriteJob> =>
						api.v3.GET(
							v3('/api/v3/library/albums/{album_id}/edition/tags/{job_id}', {
								path: { album_id: localId, job_id: jobId }
							}),
							{ signal }
						)
				: skipToken
		};
	});

// A write or undo that was applied runs on after the sheet closes, so its end
// is watched here: then everything that shows the files is stale, whether it
// finished or failed part way. A failed read is tried again, unless the server
// says the job cannot be read. The reads slow down, and a job still moving at
// the limit gets its refresh and is no longer read. The watch ends with the
// session.
function refreshWhenTagWriteEnds(target: EditionTarget, jobId: string): void {
	const { userId, localId } = target;
	const queryKey = LibraryQueryKeyFactory.catalog.editionTagWrite(userId, localId, jobId);
	const giveUpAt = Date.now() + TAG_WRITE_WATCH_LIMIT_MS;
	const poll = async (wait: number): Promise<void> => {
		if (authStore.user?.id !== userId) return;
		let ended = Date.now() >= giveUpAt;
		try {
			const job = await queryClient.fetchQuery({
				queryKey,
				retry: false,
				staleTime: 0,
				queryFn: ({ signal }): Promise<TagWriteJob> =>
					api.v3.GET(
						v3('/api/v3/library/albums/{album_id}/edition/tags/{job_id}', {
							path: { album_id: localId, job_id: jobId }
						}),
						{ signal }
					)
			});
			ended ||= !tagWriteMoving(job);
		} catch (error) {
			ended ||= tagWriteUnreadable(error);
		}
		if (!ended) {
			const next = Math.min(wait * TAG_WRITE_WATCH_SLOWDOWN, TAG_WRITE_WATCH_MAX_POLL_MS);
			setTimeout(() => void poll(next), wait);
			return;
		}
		if (authStore.user?.id === userId) await applyEffects(...heldEditionScope(target));
	};
	setTimeout(() => void poll(TAG_WRITE_POLL_MS), TAG_WRITE_POLL_MS);
}

export function previewEditionTags() {
	return createAppMutation(() => ({
		effects: () => [],
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
			return api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/edition/tags', { path: { album_id: localId } }),
				{
					release_mbid: releaseMbid,
					expected_identity_revision: identityRevision
				}
			);
		}
	}));
}

export function writeEditionTags() {
	return createAppMutation(() => ({
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
			api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/edition/tags/{job_id}/apply', {
					path: { album_id: localId, job_id: jobId }
				}),
				{
					preview_token: previewToken,
					expected_row_revision: rowRevision,
					idempotency_key: `apply-${jobId}`
				}
			),
		onSuccess: (_job, variables) => refreshWhenTagWriteEnds(variables, variables.jobId),
		effects: (_job, variables) => heldEditionScope(variables)
	}));
}

export function undoTagWrite() {
	return createAppMutation(() => ({
		mutationFn: ({
			localId,
			jobId,
			rowRevision
		}: EditionTarget & { jobId: string; rowRevision: number }): Promise<TagWriteQueued> =>
			api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/edition/tags/{job_id}/undo', {
					path: { album_id: localId, job_id: jobId }
				}),
				{
					expected_row_revision: rowRevision,
					idempotency_key: `undo-${jobId}`
				}
			),
		effects: (_queued, variables) => heldEditionScope(variables)
	}));
}

export function undoAlbumEdition() {
	return createAppMutation(() => ({
		mutationFn: ({ localId, identityRevision }: GuardedEditionTarget) =>
			api.v3.POST(
				v3('/api/v3/library/albums/{album_id}/edition/undo', { path: { album_id: localId } }),
				{
					expected_identity_revision: identityRevision
				}
			),
		// Identity reports list this album's change too.
		effects: (_d, variables) => [
			...heldEditionScope(variables),
			identityPreparationsChanged(variables.userId ?? undefined),
			repairsChanged()
		]
	}));
}
