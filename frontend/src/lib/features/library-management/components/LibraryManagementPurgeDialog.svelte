<!--
	Purging the original baselines: the panel with its button, and the dialog
	that shows what the purge removes and asks for the typed phrase.
-->
<script lang="ts">
	import { ArchiveRestore } from 'lucide-svelte';

	import { LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE } from '$lib/features/library-management/queries/LibraryManagementConfirmation';
	import {
		createPreviewLibraryManagementBaselinePurgeMutation,
		createPurgeLibraryManagementBaselinesMutation
	} from '$lib/features/library-management/queries/LibraryManagementMutations.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import { createUuid } from '$lib/utils/uuid';

	interface Props {
		/** Shows why the impact check failed, beside the tab's other errors. */
		onerror: (message: string) => void;
	}

	let { onerror }: Props = $props();

	const purgeImpact = createPreviewLibraryManagementBaselinePurgeMutation();
	const purgeBaselines = createPurgeLibraryManagementBaselinesMutation();

	let purgeOpen = $state(false);

	const blocked = $derived(
		Boolean(purgeImpact.data?.blocked_journal_count || purgeImpact.data?.active_restore_count)
	);

	async function openPurge(): Promise<void> {
		try {
			await purgeImpact.mutateAsync();
			purgeOpen = true;
		} catch (error) {
			onerror(error instanceof Error ? error.message : 'Could not inspect baseline usage.');
		}
	}

	async function confirmPurge(): Promise<void> {
		const impact = purgeImpact.data;
		if (!impact) return;
		await purgeBaselines.mutateAsync({
			impact_token: impact.impact_token,
			typed_confirmation: LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE,
			idempotency_key: createUuid()
		});
	}
</script>

<Card variant="bordered" padding="sm" class="sm:col-span-2">
	<div class="flex flex-wrap items-center justify-between gap-3">
		<div>
			<strong class="text-sm">Original baselines</strong>
			<p class="text-xs text-muted">
				Independent of Undo. Purging permanently removes the oldest restoration point.
			</p>
		</div>
		<Button
			variant="danger-ghost"
			size="sm"
			icon={ArchiveRestore}
			disabled={purgeImpact.isPending}
			onclick={() => void openPurge()}>Purge baselines...</Button
		>
	</div>
</Card>

<ConfirmDialog
	bind:open={purgeOpen}
	title="Purge original baselines?"
	confirmLabel="Purge baselines"
	tone="danger"
	focusTitle
	phrase={LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}
	pending={purgeBaselines.isPending}
	canConfirm={!blocked}
	onconfirm={confirmPurge}
>
	{#if purgeImpact.data}
		<p class="text-sm">
			This permanently removes <strong
				>{purgeImpact.data.baseline_count.toLocaleString()} baselines</strong
			>
			and may clean {purgeImpact.data.referenced_blob_count.toLocaleString()} unreferenced snapshot files.
		</p>
		{#if blocked}
			<div class="alert alert-error mt-3 text-sm">
				Recovery or restore work is active. Purge is blocked.
			</div>
		{/if}
	{/if}
</ConfirmDialog>
