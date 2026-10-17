<!--
	Purging the original baselines: the panel with its button, and the dialog
	that shows what the purge removes and asks for the typed phrase.
-->
<script lang="ts">
	import { ArchiveRestore } from 'lucide-svelte';

	import { LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE } from '$lib/queries/library-management/LibraryManagementConfirmation';
	import {
		previewLibraryManagementBaselinePurgeMutation,
		purgeLibraryManagementBaselinesMutation
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
	import { createUuid } from '$lib/utils/uuid';

	interface Props {
		/** Shows why the impact check failed, beside the tab's other errors. */
		onerror: (message: string) => void;
	}

	let { onerror }: Props = $props();

	const purgeImpact = previewLibraryManagementBaselinePurgeMutation();
	const purgeBaselines = purgeLibraryManagementBaselinesMutation();

	let purgeDialog: HTMLDialogElement;
	let purgeHeading: HTMLHeadingElement;
	let purgeOpener: HTMLElement | null = null;
	let purgePhrase = $state('');
	let purgeError = $state('');

	const blocked = $derived(
		Boolean(purgeImpact.data?.blocked_journal_count || purgeImpact.data?.active_restore_count)
	);

	async function openPurge(opener: HTMLElement): Promise<void> {
		purgePhrase = '';
		purgeError = '';
		purgeOpener = opener;
		try {
			await purgeImpact.mutateAsync();
			purgeDialog.showModal();
			purgeHeading.focus();
		} catch (error) {
			onerror(error instanceof Error ? error.message : 'Could not inspect baseline usage.');
		}
	}

	async function confirmPurge(): Promise<void> {
		const impact = purgeImpact.data;
		if (!impact || purgePhrase !== LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE) return;
		try {
			await purgeBaselines.mutateAsync({
				impact_token: impact.impact_token,
				typed_confirmation: purgePhrase,
				idempotency_key: createUuid()
			});
			purgeDialog.close();
		} catch (error) {
			purgeError = error instanceof Error ? error.message : 'Could not purge the baselines.';
		}
	}
</script>

<Card variant="bordered" padding="sm" class="sm:col-span-2">
	<div class="flex flex-wrap items-center justify-between gap-3">
		<div>
			<strong class="text-sm">Original baselines</strong>
			<p class="text-xs text-base-content/55">
				Independent of Undo. Purging permanently removes the oldest restoration point.
			</p>
		</div>
		<Button
			variant="danger-ghost"
			size="sm"
			icon={ArchiveRestore}
			disabled={purgeImpact.isPending}
			onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
				void openPurge(event.currentTarget)}>Purge baselines...</Button
		>
	</div>
</Card>

<dialog
	bind:this={purgeDialog}
	class="modal"
	aria-labelledby="purge-management-baselines-title"
	onclose={() => purgeOpener?.focus()}
	oncancel={(event) => {
		if (purgeBaselines.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-lg">
		<h2
			bind:this={purgeHeading}
			id="purge-management-baselines-title"
			tabindex="-1"
			class="text-lg font-bold"
		>
			Purge original baselines?
		</h2>
		{#if purgeImpact.data}<div class="mt-3 space-y-3 text-sm">
				<p>
					This permanently removes <strong
						>{purgeImpact.data.baseline_count.toLocaleString()} baselines</strong
					>
					and may clean {purgeImpact.data.referenced_blob_count.toLocaleString()} unreferenced snapshot
					files.
				</p>
				{#if blocked}<div class="alert alert-error">
						Recovery or restore work is active. Purge is blocked.
					</div>{/if}<label class="grid gap-1.5"
					><span>Type <strong>{LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}</strong></span><Input
						label={`Type ${LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}`}
						bind:value={purgePhrase}
						autocomplete="off"
					/></label
				>
			</div>{/if}
		{#if purgeError}<div class="alert alert-error mt-3 text-sm" role="alert">
				{purgeError}
			</div>{/if}
		<div class="modal-action">
			<Button
				variant="ghost"
				disabled={purgeBaselines.isPending}
				onclick={() => purgeDialog.close()}>Cancel</Button
			><Button
				variant="danger"
				loading={purgeBaselines.isPending}
				disabled={purgePhrase !== LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE || blocked}
				onclick={() => void confirmPurge()}>Purge baselines</Button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Cancel baseline purge" disabled={purgeBaselines.isPending}>close</button>
	</form>
</dialog>
