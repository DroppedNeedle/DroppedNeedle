<script lang="ts">
	import { Trash2 } from 'lucide-svelte';

	import { createDiscardLibraryManagementPreviewMutation } from '$lib/features/library-management/queries/LibraryManagementMutations.svelte';
	import { forgetLibraryManagementPreviewToken } from '$lib/features/library-management/queries/LibraryManagementPreviewTokens';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';

	interface Props {
		jobId: string;
		expectedRevision: number;
		profileName: string;
		compact?: boolean;
		ondiscard?: () => void | Promise<void>;
	}

	let { jobId, expectedRevision, profileName, compact = false, ondiscard }: Props = $props();
	let shown = $state(false);

	const discardPreview = createDiscardLibraryManagementPreviewMutation();

	async function discard(): Promise<void> {
		const completeDiscard = ondiscard;
		await discardPreview.mutateAsync({
			jobId,
			request: { expected_operation_row_revision: expectedRevision }
		});
		forgetLibraryManagementPreviewToken(jobId);
		await completeDiscard?.();
	}
</script>

<Button
	variant="danger-ghost"
	size="sm"
	class={compact ? 'shrink-0' : ''}
	icon={Trash2}
	aria-label={compact ? `Discard preview for ${profileName}` : undefined}
	disabled={discardPreview.isPending}
	onclick={() => (shown = true)}
>
	{compact ? 'Discard' : 'Discard preview...'}
</Button>

<ConfirmDialog
	bind:open={shown}
	title="Discard this preview?"
	confirmLabel="Discard preview"
	cancelLabel="Keep preview"
	tone="danger"
	icon={Trash2}
	focusTitle
	pending={discardPreview.isPending}
	onconfirm={discard}
>
	<p class="text-sm text-muted">
		This removes <strong>{profileName}</strong> from Ready previews and permanently prevents Apply. No
		music file, tag, baseline, or completed operation is changed.
	</p>
	<p class="mt-2 text-xs text-muted">
		The discarded plan remains in organization history as an audit record.
	</p>
</ConfirmDialog>
