// The private apply token of a sealed preview. It lives in session storage
// from the moment the preview was made; after a browser restart it is gone,
// so a ready preview asks the server to reissue it once per job.
import { reissueLibraryManagementPreviewMutation } from '$lib/queries/library-management/LibraryManagementMutations.svelte';
import {
	readLibraryManagementPreviewToken,
	rememberLibraryManagementPreviewToken
} from '$lib/queries/library-management/LibraryManagementPreviewTokens';
import type { LibraryManagementPreviewDetailResponse } from '$lib/queries/library-management/types';

export interface PreviewToken {
	readonly value: string | null;
	/** Reissue was refused; `retry` asks again. */
	readonly failed: boolean;
	/** A reissue for this job is running or has finished. */
	readonly resuming: boolean;
	retry(): void;
}

/** Call during component setup: it registers an effect. */
export function createPreviewToken(
	getJobId: () => string,
	getPreview: () => LibraryManagementPreviewDetailResponse | null,
	isActivation: () => boolean
): PreviewToken {
	const reissue = reissueLibraryManagementPreviewMutation();
	let reissuedToken = $state<string | null>(null);
	let attemptedJobId = $state<string | null>(null);
	let failed = $state(false);
	const value = $derived(reissuedToken ?? readLibraryManagementPreviewToken(getJobId()));

	$effect(() => {
		const jobId = getJobId();
		if (attemptedJobId !== null && attemptedJobId !== jobId) {
			reissuedToken = null;
			failed = false;
			attemptedJobId = null;
			return;
		}
		const detail = getPreview();
		if (
			detail === null ||
			attemptedJobId === jobId ||
			isActivation() ||
			detail.state !== 'ready' ||
			detail.expired ||
			detail.stale ||
			(reissuedToken ?? readLibraryManagementPreviewToken(jobId)) !== null
		)
			return;
		attemptedJobId = jobId;
		void resume(jobId);
	});

	async function resume(jobId: string): Promise<void> {
		try {
			const handle = await reissue.mutateAsync({ jobId });
			rememberLibraryManagementPreviewToken(handle.job_id, handle.preview_token);
			if (handle.job_id === getJobId()) reissuedToken = handle.preview_token;
		} catch {
			if (jobId === getJobId()) failed = true;
		}
	}

	return {
		get value() {
			return value;
		},
		get failed() {
			return failed;
		},
		get resuming() {
			return attemptedJobId === getJobId();
		},
		retry() {
			if (!getPreview()?.ready_for_confirmation) return;
			failed = false;
			attemptedJobId = null;
		}
	};
}
