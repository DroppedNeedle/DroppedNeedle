import { createMutation } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import type { RequestKind } from '$lib/constants';
import { notifyPendingApprovalCountChanged } from '$lib/utils/requestsApi';

import { invalidateQueriesWithPersister } from '../QueryClient';
import { DownloadQueryKeyFactory } from '../downloads/DownloadQueryKeyFactory';
import { REQUESTS_ENDPOINTS } from './endpoints';
import { RequestQueryKeyFactory } from './RequestQueryKeyFactory';

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
async function invalidateRequestSurface(mbids: string[] = []): Promise<void> {
	await invalidateQueriesWithPersister({ queryKey: RequestQueryKeyFactory.all });
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.all });
	for (const mbid of mbids) {
		await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.album(mbid) });
	}
}

function sameUser(contextUserId: string | undefined): boolean {
	return !!contextUserId && authStore.user?.id === contextUserId;
}

export const createCancelRequestMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.global.v3.DELETE(REQUESTS_ENDPOINTS.cancel(vars.mbid, vars.kind)),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (data, vars, context) => {
			if (!sameUser(context.userId)) return;
			// Track rows carry no album context in v3, so only album rows refresh
			// a library key.
			await invalidateRequestSurface(vars.kind === 'album' ? [vars.mbid] : []);
			notifyPendingApprovalCountChanged();
			toastStore.show({ message: data.message || 'Request cancelled', type: 'info' });
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Could not cancel that request'), type: 'error' })
	}));

export const createRetryRequestMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.global.v3.POST(REQUESTS_ENDPOINTS.retry(vars.mbid, vars.kind)),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (data, vars, context) => {
			if (!sameUser(context.userId)) return;
			await invalidateRequestSurface(vars.kind === 'album' ? [vars.mbid] : []);
			notifyPendingApprovalCountChanged();
			toastStore.show({ message: data.message || 'Request retried', type: 'success' });
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Could not retry that request'), type: 'error' })
	}));

export const createApproveRequestMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.global.v3.POST(REQUESTS_ENDPOINTS.approve(vars.mbid, vars.kind)),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (data, vars, context) => {
			if (!sameUser(context.userId)) return;
			await invalidateRequestSurface(vars.kind === 'album' ? [vars.mbid] : []);
			notifyPendingApprovalCountChanged();
			toastStore.show({ message: data.message || 'Request approved', type: 'success' });
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Could not approve that request'), type: 'error' })
	}));

export const createRejectRequestMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.global.v3.POST(REQUESTS_ENDPOINTS.reject(vars.mbid, vars.kind)),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (data, vars, context) => {
			if (!sameUser(context.userId)) return;
			await invalidateRequestSurface(vars.kind === 'album' ? [vars.mbid] : []);
			notifyPendingApprovalCountChanged();
			toastStore.show({ message: data.message || 'Request rejected', type: 'info' });
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Could not reject that request'), type: 'error' })
	}));

export const createBatchCancelRequestsMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: BatchCancelVars) =>
			api.global.v3.POST(REQUESTS_ENDPOINTS.batchCancel(), {
				musicbrainz_ids: vars.mbids,
				kind: vars.kind
			}),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (data, vars, context) => {
			if (!sameUser(context.userId)) return;
			await invalidateRequestSurface(vars.kind === 'album' ? vars.mbids : []);
			notifyPendingApprovalCountChanged();
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
			toastStore.show({ message: errorMessage(err, 'Could not cancel those requests'), type: 'error' })
	}));

export const createClearHistoryMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: RequestActionVars) =>
			api.global.v3.DELETE(REQUESTS_ENDPOINTS.clearHistory(vars.mbid, vars.kind)),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (_data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			await invalidateRequestSurface();
			toastStore.show({ message: 'Removed from history', type: 'info' });
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Could not remove that item'), type: 'error' })
	}));

export const createSyncRequestsMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.v3.POST(REQUESTS_ENDPOINTS.sync()),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: async (data, _vars, context) => {
			if (!sameUser(context.userId)) return;
			await invalidateRequestSurface();
			notifyPendingApprovalCountChanged();
			toastStore.show({
				message:
					data.reconciled > 0
						? `Synced ${data.reconciled} request${data.reconciled === 1 ? '' : 's'}`
						: 'Requests already in sync',
				type: 'info'
			});
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Could not sync requests'), type: 'error' })
	}));
