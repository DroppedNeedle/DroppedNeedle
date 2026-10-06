import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { invalidateQueriesWithPersister, queryClient } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import { scanRootsFor, toRunRequested } from './libraryScanAdapters';
import type {
	IdentificationControlResponse,
	OperationResponse,
	ScanControlResponse,
	ScanKind,
	ScanRunRequestedResponse,
	TargetLibrarySettingsResponse
} from './LibraryOperationsTypes';

async function invalidateWork(): Promise<void> {
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.activityPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.operationsPrefix() })
	]);
}

// v3 starts one scan per root and has no scan kinds or policy-revision
// guard: every kind runs as a plain scan over the roots the scope ids name.
export function requestLibraryRun() {
	return createMutation(() => ({
		mutationFn: async (input: {
			kind: ScanKind;
			scope_ids: string[];
			expected_policy_revision: string;
		}): Promise<ScanRunRequestedResponse> => {
			const settings = queryClient.getQueryData<TargetLibrarySettingsResponse>(
				LibraryQueryKeyFactory.targetSettings()
			);
			const responses = await Promise.all(
				scanRootsFor(input.scope_ids, settings).map((rootId) =>
					api.global.v3.POST(LibraryV3Api.scan(), { root_id: rootId })
				)
			);
			return toRunRequested(responses[0]);
		},
		onSuccess: async () => {
			await invalidateWork();
			toastStore.show({ message: 'Library work queued', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not queue library work', type: 'error' })
	}));
}

export function controlLibraryRun(action: 'pause' | 'resume' | 'stop') {
	return createMutation(() => ({
		mutationFn: (input: { runId: string; expectedRevision: number }) => {
			const url =
				action === 'pause'
					? API.library.pauseScanRun(input.runId)
					: action === 'resume'
						? API.library.resumeScanRun(input.runId)
						: API.library.stopScanRun(input.runId);
			return api.global.post<ScanControlResponse>(url, {
				expected_revision: input.expectedRevision
			});
		},
		onSuccess: invalidateWork,
		onError: () => toastStore.show({ message: `Could not ${action} the scan`, type: 'error' })
	}));
}

export function controlIdentification(action: 'pause' | 'resume') {
	return createMutation(() => ({
		mutationFn: (expectedRevision: number) =>
			api.global.post<IdentificationControlResponse>(
				action === 'pause' ? API.library.pauseIdentification() : API.library.resumeIdentification(),
				{ expected_revision: expectedRevision }
			),
		onSuccess: invalidateWork,
		onError: () => toastStore.show({ message: `Could not ${action} identification`, type: 'error' })
	}));
}

export function controlLibraryOperation(action: 'pause' | 'resume' | 'stop') {
	return createMutation(() => ({
		mutationFn: (input: { jobId: string; expectedRevision: number }) => {
			const url =
				action === 'pause'
					? API.library.pauseOperation(input.jobId)
					: action === 'resume'
						? API.library.resumeOperation(input.jobId)
						: API.library.stopOperation(input.jobId);
			return api.global.post<OperationResponse>(url, {
				expected_row_revision: input.expectedRevision
			});
		},
		onSuccess: invalidateWork,
		onError: () => toastStore.show({ message: `Could not ${action} this job`, type: 'error' })
	}));
}
