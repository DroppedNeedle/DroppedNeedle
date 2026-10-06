import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import { toOperationResponse } from './libraryOperationAdapters';
import type {
	IdentificationControlResponse,
	LibraryWorkState,
	OperationResponse,
	ScanControlResponse,
	ScanKind,
	ScanRunRequestedResponse
} from './LibraryOperationsTypes';

async function invalidateWork(): Promise<void> {
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.activityPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.operationsPrefix() })
	]);
}

export function requestLibraryRun() {
	return createMutation(() => ({
		mutationFn: async (input: {
			kind: ScanKind;
			scope_ids: string[];
			expected_policy_revision: string;
		}): Promise<ScanRunRequestedResponse> => {
			const response = await api.global.v3.POST(LibraryV3Api.scanRuns(), input);
			return {
				run_id: response.run_id,
				disposition: response.disposition,
				state: response.state as LibraryWorkState,
				row_revision: response.row_revision,
				queued_reason: response.queued_reason ?? null,
				conflicting_kind: (response.conflicting_kind ?? null) as ScanKind | null,
				estimated_file_count: null
			};
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
		mutationFn: async (input: {
			runId: string;
			expectedRevision: number;
		}): Promise<ScanControlResponse> => {
			const url =
				action === 'pause'
					? LibraryV3Api.pauseScanRun(input.runId)
					: action === 'resume'
						? LibraryV3Api.resumeScanRun(input.runId)
						: LibraryV3Api.stopScanRun(input.runId);
			const response = await api.global.v3.POST(url, {
				expected_revision: input.expectedRevision
			});
			return { ...response, state: response.state as LibraryWorkState };
		},
		onSuccess: invalidateWork,
		onError: () => toastStore.show({ message: `Could not ${action} the scan`, type: 'error' })
	}));
}

export function controlIdentification(action: 'pause' | 'resume') {
	return createMutation(() => ({
		mutationFn: (expectedRevision: number): Promise<IdentificationControlResponse> =>
			api.global.v3.POST(
				action === 'pause'
					? LibraryV3Api.pauseIdentification()
					: LibraryV3Api.resumeIdentification(),
				{ expected_revision: expectedRevision }
			),
		onSuccess: invalidateWork,
		onError: () => toastStore.show({ message: `Could not ${action} identification`, type: 'error' })
	}));
}

export function operationControlUrl(action: 'pause' | 'resume' | 'stop', jobId: string) {
	if (action === 'pause') return LibraryV3Api.pauseOperation(jobId);
	if (action === 'resume') return LibraryV3Api.resumeOperation(jobId);
	return LibraryV3Api.stopOperation(jobId);
}

export function controlLibraryOperation(action: 'pause' | 'resume' | 'stop') {
	return createMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			expectedRevision: number;
		}): Promise<OperationResponse> =>
			toOperationResponse(
				await api.global.v3.POST(operationControlUrl(action, input.jobId), {
					expected_row_revision: input.expectedRevision
				})
			),
		onSuccess: invalidateWork,
		onError: () => toastStore.show({ message: `Could not ${action} this job`, type: 'error' })
	}));
}
