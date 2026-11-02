import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import type {
	IdentificationControlResponse,
	OperationResponse,
	ScanControlResponse,
	ScanKind,
	ScanRunRequestedResponse
} from './LibraryOperationsTypes';
import {
	anyLibraryActivityChanged,
	createAppMutation,
	libraryOperationsChanged
} from '$lib/queries/effects';

const work = () => [anyLibraryActivityChanged(), libraryOperationsChanged()];

export function requestLibraryRun() {
	return createAppMutation(() => ({
		mutationFn: (input: {
			kind: ScanKind;
			scope_ids: string[];
			expected_policy_revision: string;
		}): Promise<ScanRunRequestedResponse> => api.v3.POST(v3('/api/v3/library/scan/runs'), input),
		effects: work,
		onSuccess: () => {
			toastStore.show({ message: 'Library work queued', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not queue library work', type: 'error' })
	}));
}

export function controlLibraryRun(action: 'pause' | 'resume' | 'stop') {
	return createAppMutation(() => ({
		mutationFn: (input: {
			runId: string;
			expectedRevision: number;
		}): Promise<ScanControlResponse> => {
			const url =
				action === 'pause'
					? v3('/api/v3/library/scan/runs/{id}/pause', { path: { id: input.runId } })
					: action === 'resume'
						? v3('/api/v3/library/scan/runs/{id}/resume', { path: { id: input.runId } })
						: v3('/api/v3/library/scan/runs/{id}/stop', { path: { id: input.runId } });
			return api.v3.POST(url, { expected_revision: input.expectedRevision });
		},
		effects: work,
		onError: () => toastStore.show({ message: `Could not ${action} the scan`, type: 'error' })
	}));
}

export function controlIdentification(action: 'pause' | 'resume') {
	return createAppMutation(() => ({
		mutationFn: (expectedRevision: number): Promise<IdentificationControlResponse> =>
			api.v3.POST(
				action === 'pause'
					? v3('/api/v3/library/identification/pause')
					: v3('/api/v3/library/identification/resume'),
				{ expected_revision: expectedRevision }
			),
		effects: work,
		onError: () => toastStore.show({ message: `Could not ${action} identification`, type: 'error' })
	}));
}

export function operationControlUrl(action: 'pause' | 'resume' | 'stop', jobId: string) {
	if (action === 'pause')
		return v3('/api/v3/library/operations/{job_id}/pause', { path: { job_id: jobId } });
	if (action === 'resume')
		return v3('/api/v3/library/operations/{job_id}/resume', { path: { job_id: jobId } });
	return v3('/api/v3/library/operations/{job_id}/stop', { path: { job_id: jobId } });
}

export function controlLibraryOperation(action: 'pause' | 'resume' | 'stop') {
	return createAppMutation(() => ({
		mutationFn: async (input: {
			jobId: string;
			expectedRevision: number;
		}): Promise<OperationResponse> =>
			await api.v3.POST(operationControlUrl(action, input.jobId), {
				expected_row_revision: input.expectedRevision
			}),
		effects: work,
		onError: () => toastStore.show({ message: `Could not ${action} this job`, type: 'error' })
	}));
}

/** A run's diagnostic report, as a file to save, and the name the server gave it. */
export const createScanDiagnosticsExportMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: async (runId: string): Promise<{ blob: Blob; filename: string }> => {
			const response = await api.raw(
				v3('/api/v3/library/scan/runs/{id}/diagnostics', { path: { id: runId } })
			);
			if (!response.ok) throw new Error('Diagnostic export failed');
			const disposition = response.headers.get('content-disposition') ?? '';
			return {
				blob: await response.blob(),
				filename: disposition.match(/filename="([^"]+)"/)?.[1] ?? 'droppedneedle-library-run.json'
			};
		}
	}));
