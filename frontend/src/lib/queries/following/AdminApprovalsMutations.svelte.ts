import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { toastStore } from '$lib/stores/toast';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { FollowQueryKeyFactory } from './FollowQueryKeyFactory';
import { FOLLOW_ENDPOINTS } from './endpoints';
import { notifyPendingApprovalCountChanged } from '$lib/utils/requestsApi';
import type { ApprovalActionResponse } from './types';

interface ApprovalActionVars {
	userId: string;
	mbid: string;
	artistName: string;
}

function invalidateApprovals(): Promise<void> {
	return invalidateQueriesWithPersister({ queryKey: FollowQueryKeyFactory.adminApprovals() });
}

function errorMessage(err: unknown, fallback: string): string {
	return err instanceof Error && err.message ? err.message : fallback;
}

export const createApproveAutoDownloadMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ApprovalActionVars) =>
			api.global.post<ApprovalActionResponse>(FOLLOW_ENDPOINTS.approve(vars.userId, vars.mbid)),
		onSuccess: async (_data, vars) => {
			toastStore.show({
				message: `Auto-download approved for ${vars.artistName}`,
				type: 'success'
			});
			await invalidateApprovals();
			notifyPendingApprovalCountChanged();
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Approve failed'), type: 'error' })
	}));

export const createRejectAutoDownloadMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ApprovalActionVars) =>
			api.global.post<ApprovalActionResponse>(FOLLOW_ENDPOINTS.reject(vars.userId, vars.mbid)),
		onSuccess: async (_data, vars) => {
			toastStore.show({ message: `Auto-download rejected for ${vars.artistName}`, type: 'info' });
			await invalidateApprovals();
			notifyPendingApprovalCountChanged();
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Reject failed'), type: 'error' })
	}));

// Bulk "Lidarr Import" approval batches (LidarrImport D3). Invalidating the adminApprovals
// prefix sweeps the nested batches key too.
interface BatchActionVars {
	batchId: string;
	userName: string;
	artistCount: number;
}

export const createApproveAutoDownloadBatchMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: BatchActionVars) =>
			api.global.post<ApprovalActionResponse>(FOLLOW_ENDPOINTS.approveBatch(vars.batchId)),
		onSuccess: async (_data, vars) => {
			toastStore.show({
				message: `Auto-download approved for ${vars.userName}'s ${vars.artistCount} artists`,
				type: 'success'
			});
			await invalidateApprovals();
			notifyPendingApprovalCountChanged();
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Approve failed'), type: 'error' })
	}));

export const createRejectAutoDownloadBatchMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: BatchActionVars) =>
			api.global.post<ApprovalActionResponse>(FOLLOW_ENDPOINTS.rejectBatch(vars.batchId)),
		onSuccess: async (_data, vars) => {
			toastStore.show({
				message: `Auto-download rejected for ${vars.userName}'s ${vars.artistCount} artists`,
				type: 'info'
			});
			await invalidateApprovals();
			notifyPendingApprovalCountChanged();
		},
		onError: (err) =>
			toastStore.show({ message: errorMessage(err, 'Reject failed'), type: 'error' })
	}));
