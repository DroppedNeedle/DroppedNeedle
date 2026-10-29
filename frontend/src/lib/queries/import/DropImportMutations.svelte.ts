import { api } from '$lib/api/client';
import { ApiError, TransportError } from '$lib/api/errors';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';

import { DROP_IMPORT_ENDPOINTS } from './endpoints';
import type { DropImportJob } from './types';
import { createAppMutation, dropImportsChanged } from '$lib/queries/effects';

/** An upload that sends nothing for this long has stalled. */
const STALL_MS = 60_000;

const PROXY_HINT = 'If you use a reverse proxy, check its upload limit.';

/** What went wrong with an upload, in words a person can act on. */
function uploadFailure(error: Error): string {
	if (error instanceof TransportError) {
		if (error.code === 'TRANSPORT_STALLED') return `The upload stopped. ${PROXY_HINT}`;
		if (error.code === 'TRANSPORT_NETWORK') return `The upload was cut off. ${PROXY_HINT}`;
		return error.message;
	}
	if (error instanceof ApiError && !error.code) {
		// Not our error envelope: a proxy in front of the server answered.
		return error.status === 413
			? 'A proxy refused the upload as too large. Raise its upload limit.'
			: `The upload failed (status ${error.status}). ${PROXY_HINT}`;
	}
	return error.message || 'Upload failed.';
}

export interface DropUpload {
	files: File[];
	onProgress: (sent: number, total: number) => void;
}

export const uploadDropMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ files, onProgress }: DropUpload) => {
			const form = new FormData();
			for (const file of files) form.append('files', file, file.name);
			return api.upload<DropImportJob>(DROP_IMPORT_ENDPOINTS.uploads(), form, {
				onProgress,
				stallMs: STALL_MS
			});
		},
		onSuccess: () => {
			toastStore.show({ message: 'Import started - identifying your files.', type: 'success' });
		},
		effects: () => [dropImportsChanged(authStore.user?.id)],
		onError: (error: Error) => {
			toastStore.show({ message: uploadFailure(error), type: 'error', duration: 8000 });
		}
	}));
