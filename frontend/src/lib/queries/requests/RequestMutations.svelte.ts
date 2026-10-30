import { api } from '$lib/api/client';
import { ApiError } from '$lib/api/errors';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import type { RequestKind } from './types';

import { REQUESTS_ENDPOINTS } from './endpoints';
import {
	albumStatusChanged,
	anyDownloadChanged,
	anyRequestChanged,
	applyEffects,
	createAppMutation,
	type Effect,
	pendingApprovalsChanged
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
			api.v3.DELETE(REQUESTS_ENDPOINTS.cancel(vars.mbid, vars.kind)),
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
			api.v3.POST(REQUESTS_ENDPOINTS.retry(vars.mbid, vars.kind)),
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
			toastStore.show({
				message: errorMessage(err, 'Could not retry that request'),
				type: 'error'
			});
		}
	}));

// A single found only on its album: fetch it again and take the album's
// track as the song.
export const createFromAlbumMutation = () =>
	createAppMutation(() => ({
		mutationFn: (recordingMbid: string) => api.v3.POST(REQUESTS_ENDPOINTS.fromAlbum(recordingMbid)),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, _mbid, context) => (sameUser(context.userId) ? requestSurface([], false) : []),
		onSuccess: (data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show({ message: data.message || 'Getting it from the album', type: 'success' });
		},
		onError: (err) =>
			toastStore.show({
				message: errorMessage(err, 'Could not get it from the album'),
				type: 'error'
			})
	}));

export const createApproveRequestMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.v3.POST(REQUESTS_ENDPOINTS.approve(vars.mbid, vars.kind)),
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
			api.v3.POST(REQUESTS_ENDPOINTS.reject(vars.mbid, vars.kind)),
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

export const createBatchCancelRequestsMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: BatchCancelVars) =>
			api.v3.POST(REQUESTS_ENDPOINTS.batchCancel(), {
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
			api.v3.DELETE(REQUESTS_ENDPOINTS.clearHistory(vars.mbid, vars.kind)),
		onMutate: () => ({ userId: authStore.user?.id }),
		effects: (_data, _vars, context) => (sameUser(context.userId) ? requestSurface([], false) : []),
		onSuccess: (_data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			toastStore.show({ message: 'Removed from history', type: 'info' });
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Could not remove that item'), type: 'error' })
	}));
