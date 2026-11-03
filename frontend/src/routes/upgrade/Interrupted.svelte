<script lang="ts">
	import { ApiError } from '$lib/api/errors';
	import {
		createUpgradeStartMutation,
		createUpgradeUndoMutation
	} from '$lib/queries/upgrade/UpgradeQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';

	const start = createUpgradeStartMutation();
	const undo = createUpgradeUndoMutation();
	let confirmUndo = $state(false);
	let error = $state<string | null>(null);

	async function finish() {
		error = null;
		try {
			await start.mutateAsync([]);
		} catch (cause) {
			error = cause instanceof ApiError ? cause.message : 'Could not reach the server';
		}
	}
</script>

<Card variant="bordered" title="An upgrade didn't finish">
	<p class="text-sm text-base-content/70">
		Finishing puts your v2 files back first, then upgrades again.
	</p>
	{#if error}
		<div class="alert alert-error mt-3 py-2 text-sm" role="alert">{error}</div>
	{/if}
	{#snippet actions()}
		<div class="flex w-full flex-col-reverse gap-2 sm:flex-row sm:justify-between">
			<Button variant="ghost" onclick={() => (confirmUndo = true)}>Go back to v2</Button>
			<Button variant="primary" loading={start.isPending} onclick={() => void finish()}>
				Finish the upgrade
			</Button>
		</div>
	{/snippet}
</Card>

<ConfirmDialog
	bind:open={confirmUndo}
	title="Go back to v2?"
	message="What the unfinished upgrade made is deleted and v2's files go back where they were."
	confirmLabel="Go back to v2"
	tone="danger"
	pending={undo.isPending}
	onconfirm={() => undo.mutateAsync()}
/>
