<!--
	The typed confirmation that applies a sealed preview. Apply sends the exact
	preview token and row revision the page was showing, so the server refuses
	it if anything changed since. A token the server no longer accepts is
	replaced once and the same apply is sent again. On success the token is
	forgotten and the page moves to the new operation.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { ApiError } from '$lib/api/errors';
	import { ShieldAlert } from 'lucide-svelte';

	import { LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE } from '$lib/features/library-management/queries/LibraryManagementConfirmation';
	import { createApplyLibraryManagementPreviewMutation } from '$lib/features/library-management/queries/LibraryManagementMutations.svelte';
	import { forgetLibraryManagementPreviewToken } from '$lib/features/library-management/queries/LibraryManagementPreviewTokens';
	import type { LibraryManagementPreviewDetailResponse } from '$lib/features/library-management/queries/types';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { createUuid } from '$lib/utils/uuid';
	import type { ManagementApplyCopy } from './LibraryManagementDisplay';

	interface Props {
		jobId: string;
		preview: LibraryManagementPreviewDetailResponse;
		token: string | null;
		/** A new token for this preview after the server refused `token`. */
		replaceToken: () => Promise<string | null>;
		canApply: boolean;
		copy: ManagementApplyCopy;
	}

	let { jobId, preview, token, replaceToken, canApply, copy }: Props = $props();
	let shown = $state(false);

	const applyPreview = createApplyLibraryManagementPreviewMutation();

	export function open(): void {
		shown = true;
	}

	async function apply(): Promise<void> {
		if (!token) return;
		const idempotencyKey = createUuid();
		const send = (previewToken: string) =>
			applyPreview.mutateAsync({
				jobId,
				request: {
					preview_token: previewToken,
					expected_operation_row_revision: preview.operation_row_revision,
					idempotency_key: idempotencyKey,
					confirmation: true
				}
			});
		let operation: Awaited<ReturnType<typeof send>>;
		try {
			operation = await send(token);
		} catch (cause) {
			if (!(cause instanceof ApiError && cause.code === 'PREVIEW_TOKEN_INVALID')) throw cause;
			const replaced = await replaceToken();
			if (!replaced) throw cause;
			operation = await send(replaced);
		}
		forgetLibraryManagementPreviewToken(jobId);
		await goto(withBasePath(`/library/management/operations/${encodeURIComponent(operation.id)}`));
	}
</script>

<ConfirmDialog
	bind:open={shown}
	title={copy.title}
	message={copy.detail}
	confirmLabel={copy.confirmButton}
	tone="warning"
	icon={ShieldAlert}
	focusTitle
	phrase={LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}
	pending={applyPreview.isPending}
	canConfirm={canApply}
	onconfirm={apply}
/>
