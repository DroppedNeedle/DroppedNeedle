<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { ShieldAlert, Trash2 } from 'lucide-svelte';

	import { discardLibraryManagementPreviewMutation } from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import { forgetLibraryManagementPreviewToken } from '$lib/queries/library-management/LibraryManagementPreviewTokens';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		jobId: string;
		expectedRevision: number;
		profileName: string;
		compact?: boolean;
		ondiscard?: () => void | Promise<void>;
	}

	let { jobId, expectedRevision, profileName, compact = false, ondiscard }: Props = $props();
	let dialog: HTMLDialogElement;
	let heading: HTMLHeadingElement;
	let opener: HTMLElement | null = null;
	let errorMessage = $state('');

	const discardPreview = discardLibraryManagementPreviewMutation();
	const titleId = $derived(`discard-management-preview-${jobId}`);

	function open(button: HTMLElement): void {
		opener = button;
		errorMessage = '';
		dialog.showModal();
		heading.focus();
	}

	async function discard(): Promise<void> {
		errorMessage = '';
		const completeDiscard = ondiscard;
		try {
			await discardPreview.mutateAsync({
				jobId,
				request: { expected_operation_row_revision: expectedRevision }
			});
			forgetLibraryManagementPreviewToken(jobId);
			await completeDiscard?.();
			if (dialog.isConnected && dialog.open) dialog.close();
		} catch (error) {
			errorMessage = error instanceof Error ? error.message : 'Could not discard this preview.';
		}
	}
</script>

<Button
	variant="danger-ghost"
	size="sm"
	class={compact ? 'shrink-0' : ''}
	icon={Trash2}
	aria-label={compact ? `Discard preview for ${profileName}` : undefined}
	disabled={discardPreview.isPending}
	onclick={(event) => open(event.currentTarget)}
>
	{compact ? 'Discard' : 'Discard preview...'}
</Button>

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby={titleId}
	onclose={() => opener?.focus()}
	oncancel={(event) => {
		if (discardPreview.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-lg border border-error/25">
		<div class="flex items-start gap-3">
			<div class="management-write-mark"><ShieldAlert class="h-5 w-5" /></div>
			<div>
				<p class="management-kicker">Read-only preview</p>
				<h2
					bind:this={heading}
					id={titleId}
					tabindex="-1"
					class="font-display text-xl font-semibold"
				>
					Discard this preview?
				</h2>
			</div>
		</div>
		<p class="mt-4 text-sm text-base-content/65">
			This removes <strong>{profileName}</strong> from Ready previews and permanently prevents Apply.
			No music file, tag, baseline, or completed operation is changed.
		</p>
		<p class="mt-2 text-xs text-base-content/50">
			The discarded plan remains in organization history as an audit record.
		</p>
		{#if errorMessage}<div class="alert alert-error mt-3 text-sm" role="alert">
				{errorMessage}
			</div>{/if}
		<div class="modal-action">
			<Button variant="ghost" disabled={discardPreview.isPending} onclick={() => dialog.close()}
				>Keep preview</Button
			>
			<Button
				variant="danger"
				icon={Trash2}
				loading={discardPreview.isPending}
				onclick={() => void discard()}>Discard preview</Button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Keep management preview" disabled={discardPreview.isPending}>close</button>
	</form>
</dialog>
