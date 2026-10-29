import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { FOLLOW_ENDPOINTS } from './endpoints';
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
			api.v3.POST(FOLLOW_ENDPOINTS.approve(vars.userId, vars.mbid)),
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
			api.v3.POST(FOLLOW_ENDPOINTS.reject(vars.userId, vars.mbid)),
		onSuccess: (_data, vars) => {
			toastStore.show({ message: `Auto-download rejected for ${vars.artistName}`, type: 'info' });
		},
		effects: approvals,
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Reject failed'), type: 'error' })
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
		mutationFn: (vars: BatchActionVars) => api.v3.POST(FOLLOW_ENDPOINTS.approveBatch(vars.batchId)),
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
		mutationFn: (vars: BatchActionVars) => api.v3.POST(FOLLOW_ENDPOINTS.rejectBatch(vars.batchId)),
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
