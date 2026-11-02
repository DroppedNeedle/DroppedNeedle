import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
import { autoDownloadApprovalsChanged, createAppMutation } from '$lib/queries/effects';

interface ApprovalActionVars {
	userId: string;
	mbid: string;
	artistName: string;
}

// The approvals prefix holds the pending count and the batches too.
const approvals = () => [autoDownloadApprovalsChanged(authStore.user?.id)];

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

export const createApproveAutoDownloadMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: ApprovalActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/approve', {
					path: { user_id: vars.userId, artist_mbid: vars.mbid }
				})
			),
		onSuccess: (_data, vars) => {
			toastStore.show({
				message: `Auto-download approved for ${vars.artistName}`,
				type: 'success'
			});
		},
		effects: approvals,
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Approve failed'), type: 'error' })
	}));

export const createRejectAutoDownloadMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: ApprovalActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/reject', {
					path: { user_id: vars.userId, artist_mbid: vars.mbid }
				})
			),
		onSuccess: (_data, vars) => {
			toastStore.show({ message: `Auto-download rejected for ${vars.artistName}`, type: 'info' });
		},
		effects: approvals,
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Reject failed'), type: 'error' })
	}));

export const createRevokeAutoDownloadMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: ApprovalActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/revoke', {
					path: { user_id: vars.userId, artist_mbid: vars.mbid }
				})
			),
		onSuccess: (data, vars) => {
			toastStore.show(
				data.success
					? { message: `Auto-download revoked for ${vars.artistName}`, type: 'info' }
					: { message: data.message, type: 'error' }
			);
		},
		effects: approvals,
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Revoke failed'), type: 'error' })
	}));

// Bulk "Lidarr Import" approval batches. Invalidating the adminApprovals
// prefix sweeps the nested batches key too.
interface BatchActionVars {
	batchId: string;
	userName: string;
	artistCount: number;
}

export const createApproveAutoDownloadBatchMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: BatchActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/auto-download-approval-batches/{batch_id}/approve', {
					path: { batch_id: vars.batchId }
				})
			),
		onSuccess: (_data, vars) => {
			toastStore.show({
				message: `Auto-download approved for ${vars.userName}'s ${vars.artistCount} artists`,
				type: 'success'
			});
		},
		effects: approvals,
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Approve failed'), type: 'error' })
	}));

export const createRejectAutoDownloadBatchMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: BatchActionVars) =>
			api.v3.POST(
				v3('/api/v3/requests/auto-download-approval-batches/{batch_id}/reject', {
					path: { batch_id: vars.batchId }
				})
			),
		onSuccess: (_data, vars) => {
			toastStore.show({
				message: `Auto-download rejected for ${vars.userName}'s ${vars.artistCount} artists`,
				type: 'info'
			});
		},
		effects: approvals,
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Reject failed'), type: 'error' })
	}));
