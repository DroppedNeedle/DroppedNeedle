import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { ApiError, isAlreadyInLibrary } from '$lib/api/errors';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import type { RequestKind } from './types';

import {
	albumStatusChanged,
	anyDownloadChanged,
	anyRequestChanged,
	applyEffects,
	createAppMutation,
	type Effect,
	pendingApprovalsChanged,
	playlistRequestStatesChanged,
	playlistSyncApprovalsChanged
} from '$lib/queries/effects';

export interface RequestActionVars {
	mbid: string;
	kind: RequestKind;
}

export interface BatchCancelVars {
	mbids: string[];
	kind: RequestKind;
}

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

// Every request mutation sweeps the requests prefix (active, history, and
// approvals read the same rows) plus the downloads prefix (the queue mirrors
// active rows). Mutations that move an album row also refresh that album's
// library key so the album page shows the new state (download-to-library).
// Those that change what waits for an admin refresh the approval count too.
function requestSurface(mbids: string[], approvals: boolean): Effect[] {
	const userId = authStore.user?.id;
	return [
		anyRequestChanged(),
		anyDownloadChanged(),
		...mbids.map((mbid) => albumStatusChanged(userId, mbid)),
		...(approvals ? [pendingApprovalsChanged(userId)] : [])
	];
}

function sameUser(contextUserId: string | undefined): boolean {
	return !!contextUserId && authStore.user?.id === contextUserId;
}

// A 404 means the request is gone from the caller's list: refresh it so the
// stale row goes too.
function refreshIfGone(err: unknown): void {
	if (err instanceof ApiError && err.status === 404) void applyEffects(anyRequestChanged());
}

export const createCancelRequestMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.v3.DELETE(
				v3('/api/v3/requests/active/{musicbrainz_id}', {
					path: { musicbrainz_id: vars.mbid },
					query: { kind: vars.kind }
				})
			),
		onMutate: () => ({ userId: authStore.user?.id }),
		// Track rows carry no album context in v3, so only album rows refresh
		// a library key.
		effects: (_data, vars, context) =>
			sameUser(context.userId)
				? requestSurface(vars.kind === 'album' ? [vars.mbid] : [], true)
				: [],
		onSuccess: (data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show({ message: data.message || 'Request cancelled', type: 'info' });
		},
		onError: (err) => {
			refreshIfGone(err);
			toastStore.show({
				message: errorMessage(err, 'Could not cancel that request'),
				type: 'error'
			});
		}
	}));

export const createRetryRequestMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/retry/{musicbrainz_id}', {
					path: { musicbrainz_id: vars.mbid },
					query: { kind: vars.kind }
				})
			),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, vars, context) =>
			sameUser(context.userId)
				? requestSurface(vars.kind === 'album' ? [vars.mbid] : [], true)
				: [],
		onSuccess: (data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show({ message: data.message || 'Request retried', type: 'success' });
		},
		onError: (err) => {
			refreshIfGone(err);
			toastStore.show(
				isAlreadyInLibrary(err)
					? { message: err.message, type: 'info' }
					: {
							message: errorMessage(err, 'Could not retry that request'),
							type: 'error'
						}
			);
		}
	}));

// A single found only on its album: fetch it again and take the album's
// track as the song.
export const createFromAlbumMutation = () =>
	createAppMutation(() => ({
		mutationFn: (recordingMbid: string) =>
			api.v3.POST(
				v3('/api/v3/requests/tracks/{musicbrainz_id}/from-album', {
					path: { musicbrainz_id: recordingMbid }
				})
			),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, _mbid, context) => (sameUser(context.userId) ? requestSurface([], false) : []),
		onSuccess: (data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show({ message: data.message || 'Getting it from the album', type: 'success' });
		},
		onError: (err) =>
			toastStore.show(
				isAlreadyInLibrary(err)
					? { message: err.message, type: 'info' }
					: {
							message: errorMessage(err, 'Could not get it from the album'),
							type: 'error'
						}
			)
	}));

export const createApproveRequestMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/approvals/{musicbrainz_id}/approve', {
					path: { musicbrainz_id: vars.mbid },
					query: { kind: vars.kind }
				})
			),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, vars, context) =>
			sameUser(context.userId)
				? requestSurface(vars.kind === 'album' ? [vars.mbid] : [], true)
				: [],
		onSuccess: (data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show({ message: data.message || 'Request approved', type: 'success' });
		},
		onError: (err) =>
			toastStore.show({
				message: errorMessage(err, 'Could not approve that request'),
				type: 'error'
			})
	}));

export const createRejectRequestMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/approvals/{musicbrainz_id}/reject', {
					path: { musicbrainz_id: vars.mbid },
					query: { kind: vars.kind }
				})
			),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, vars, context) =>
			sameUser(context.userId)
				? requestSurface(vars.kind === 'album' ? [vars.mbid] : [], true)
				: [],
		onSuccess: (data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show({ message: data.message || 'Request rejected', type: 'info' });
		},
		onError: (err) =>
			toastStore.show({
				message: errorMessage(err, 'Could not reject that request'),
				type: 'error'
			})
	}));

/**
 * Approve a card's `approve` albums and reject the rest of it, in one call.
 * `all` is every album on the card, whose pages all change.
 */
export const createDecideApprovalBatchMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: { batchId: string; approve: string[]; all: string[] }) =>
			api.v3.POST(
				v3('/api/v3/requests/approvals/batches/{batch_id}/decide', {
					path: { batch_id: vars.batchId }
				}),
				{ approve: vars.approve }
			),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, vars, context) =>
			sameUser(context.userId)
				? [...requestSurface(vars.all, true), playlistRequestStatesChanged()]
				: [],
		onSuccess: (data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show({
				message: `Approved ${data.approved}, rejected ${data.rejected}.`,
				type: data.approved > 0 ? 'success' : 'info'
			});
		},
		onError: (err) =>
			toastStore.show({
				message: errorMessage(err, 'Could not decide those requests'),
				type: 'error'
			})
	}));

export const createBatchCancelRequestsMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: BatchCancelVars) =>
			api.v3.POST(v3('/api/v3/requests/batches/cancel'), {
				musicbrainz_ids: vars.mbids,
				kind: vars.kind
			}),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, vars, context) =>
			sameUser(context.userId) ? requestSurface(vars.kind === 'album' ? vars.mbids : [], true) : [],
		onSuccess: (data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show({
				message:
					data.message ||
					(data.cancelled > 0
						? `Cancelled ${data.cancelled} request${data.cancelled === 1 ? '' : 's'}`
						: 'Nothing to cancel'),
				type: 'info'
			});
		},
		onError: (err) =>
			toastStore.show({
				message: errorMessage(err, 'Could not cancel those requests'),
				type: 'error'
			})
	}));

export const createClearHistoryMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.v3.DELETE(
				v3('/api/v3/requests/history/{musicbrainz_id}', {
					path: { musicbrainz_id: vars.mbid },
					query: { kind: vars.kind }
				})
			),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, _vars, context) => (sameUser(context.userId) ? requestSurface([], false) : []),
		// A refused clear (a status that may not be cleared) answers 200 with
		// success false.
		onSuccess: (data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show(
				data.success
					? { message: 'Removed from history', type: 'info' }
					: { message: "That request can't be removed from history", type: 'error' }
			);
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Could not remove that item'), type: 'error' })
	}));

interface PlaylistSyncDecisionVars {
	playlistId: string;
	playlistName: string;
}

type SyncVerb = 'approve' | 'reject' | 'revoke';

const SYNC_DONE: Record<SyncVerb, string> = {
	approve: 'Requests approved for',
	reject: 'Requests turned down for',
	revoke: 'Approval taken back for'
};

/** What every decision on a synced playlist's requests does after it. */
const syncDecision = (verb: SyncVerb) => ({
	onSuccess: (data: { success: boolean; message: string }, vars: PlaylistSyncDecisionVars) => {
		toastStore.show(
			data.success
				? { message: `${SYNC_DONE[verb]} ${vars.playlistName}`, type: 'success' }
				: { message: data.message, type: 'error' }
		);
	},
	effects: () => [playlistSyncApprovalsChanged(), pendingApprovalsChanged(authStore.user?.id)],
	onError: (err: unknown) =>
		toastStore.show({ message: errorMessage(err, "Couldn't save that decision"), type: 'error' })
});

export const createApprovePlaylistSyncMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: PlaylistSyncDecisionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/playlist-sync-approvals/{playlist_id}/approve', {
					path: { playlist_id: vars.playlistId }
				})
			),
		...syncDecision('approve')
	}));

export const createRejectPlaylistSyncMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: PlaylistSyncDecisionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/playlist-sync-approvals/{playlist_id}/reject', {
					path: { playlist_id: vars.playlistId }
				})
			),
		...syncDecision('reject')
	}));

export const createRevokePlaylistSyncMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: PlaylistSyncDecisionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/playlist-sync-approvals/{playlist_id}/revoke', {
					path: { playlist_id: vars.playlistId }
				})
			),
		...syncDecision('revoke')
	}));
