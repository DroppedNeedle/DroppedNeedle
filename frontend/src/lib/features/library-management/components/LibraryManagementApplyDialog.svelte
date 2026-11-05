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
	import { FolderCog, Image as ImageIcon, Layers, ShieldAlert, Tags } from 'lucide-svelte';

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
	const summary = $derived(preview.summary);

	function count(value: number, noun: string): string {
		return `${value.toLocaleString()} ${noun}${value === 1 ? '' : 's'}`;
	}

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
	paper
	phrase={LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}
	pending={applyPreview.isPending}
	canConfirm={canApply}
	onconfirm={apply}
>
	<dl class="apply-counts">
		<div>
			<dt><Tags class="size-4" aria-hidden="true" />Tags</dt>
			<dd>{count(summary.tag_change_count, 'change')}</dd>
		</div>
		<div>
			<dt><FolderCog class="size-4" aria-hidden="true" />File paths</dt>
			<dd>{count(summary.path_change_count, 'change')}</dd>
		</div>
		<div>
			<dt><ImageIcon class="size-4" aria-hidden="true" />Artwork</dt>
			<dd>{count(summary.artwork_change_count, 'change')}</dd>
		</div>
		<div>
			<dt><Layers class="size-4" aria-hidden="true" />Sidecars</dt>
			<dd>{count(summary.sidecar_change_count, 'change')}</dd>
		</div>
		<div class="apply-counts__total">
			<dt>Files to write</dt>
			<dd>{count(summary.eligible_count + summary.warning_count, 'file')}</dd>
		</div>
	</dl>
</ConfirmDialog>

<style>
	.apply-counts {
		display: grid;
		margin-top: 1rem;
		border-top: 1px solid var(--dn-line);
	}
	.apply-counts > div {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		padding: 0.5rem 0.25rem;
		border-bottom: 1px solid var(--dn-line);
		font-size: 0.9375rem;
	}
	.apply-counts dt {
		display: inline-flex;
		align-items: center;
		gap: 0.625rem;
	}
	.apply-counts dd {
		font-family: var(--font-mono);
		font-size: 0.875rem;
	}
	.apply-counts__total {
		font-weight: 700;
	}
	.apply-counts > .apply-counts__total {
		border-bottom: 0;
	}
</style>
