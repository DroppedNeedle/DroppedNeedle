<!--
	The typed confirmation that applies a sealed preview. Apply sends the exact
	preview token and row revision the page was showing, so the server refuses
	it if anything changed since. On success the token is forgotten and the
	page moves to the new operation.
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { goto } from '$app/navigation';
	import { CircleCheckBig, ShieldAlert } from 'lucide-svelte';

	import { LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE } from '$lib/queries/library-management/LibraryManagementConfirmation';
	import { applyLibraryManagementPreviewMutation } from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import { forgetLibraryManagementPreviewToken } from '$lib/queries/library-management/LibraryManagementPreviewTokens';
	import type { LibraryManagementPreviewDetailResponse } from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import Input from '$lib/ui/Input.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { createUuid } from '$lib/utils/uuid';
	import type { ManagementApplyCopy } from './LibraryManagementDisplay';

	interface Props {
		jobId: string;
		preview: LibraryManagementPreviewDetailResponse;
		token: string | null;
		canApply: boolean;
		copy: ManagementApplyCopy;
	}

	let { jobId, preview, token, canApply, copy }: Props = $props();
	let dialog: HTMLDialogElement;
	let heading: HTMLHeadingElement;
	let opener: HTMLElement | null = null;
	let confirmation = $state('');
	let error = $state('');

	const applyPreview = applyLibraryManagementPreviewMutation();
	const confirmed = $derived(confirmation === LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE);

	export function open(button: HTMLElement): void {
		opener = button;
		confirmation = '';
		error = '';
		dialog.showModal();
		heading.focus();
	}

	async function apply(): Promise<void> {
		if (!token || !confirmed || !canApply) return;
		error = '';
		try {
			const operation = await applyPreview.mutateAsync({
				jobId,
				request: {
					preview_token: token,
					expected_operation_row_revision: preview.operation_row_revision,
					idempotency_key: createUuid(),
					confirmation: true
				}
			});
			forgetLibraryManagementPreviewToken(jobId);
			dialog.close();
			await goto(
				withBasePath(`/library/management/operations/${encodeURIComponent(operation.id)}`)
			);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not apply this preview.';
		}
	}
</script>

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby="apply-management-title"
	onclose={() => opener?.focus()}
	oncancel={(event) => {
		if (applyPreview.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-lg border border-warning/30">
		<div class="flex items-start gap-3">
			<div class="management-write-mark"><ShieldAlert class="h-5 w-5" /></div>
			<div>
				<p class="management-kicker">{copy.kicker}</p>
				<h2
					bind:this={heading}
					id="apply-management-title"
					tabindex="-1"
					class="font-display text-xl font-semibold"
				>
					{copy.title}
				</h2>
			</div>
		</div>
		<p class="mt-4 text-sm text-base-content/65">{copy.detail}</p>
		<label class="mt-4 grid gap-1 text-sm"
			><span>Type <strong>{LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}</strong></span><Input
				label="Type {LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}"
				mono
				bind:value={confirmation}
				autocomplete="off"
			/></label
		>
		{#if error}<div class="alert alert-error mt-3 text-sm" role="alert">{error}</div>{/if}
		<div class="modal-action">
			<Button variant="ghost" disabled={applyPreview.isPending} onclick={() => dialog.close()}
				>Cancel</Button
			><Button
				variant="warning"
				icon={CircleCheckBig}
				loading={applyPreview.isPending}
				disabled={!canApply || !confirmed}
				onclick={() => void apply()}>{copy.confirmButton}</Button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Cancel applying management preview" disabled={applyPreview.isPending}
			>close</button
		>
	</form>
</dialog>
