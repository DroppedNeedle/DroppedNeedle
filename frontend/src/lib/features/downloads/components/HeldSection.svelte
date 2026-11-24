<script lang="ts">
	import { RotateCcw } from 'lucide-svelte';
	import { createRecheckAllHeldMutation } from '$lib/features/downloads/queries/HeldQueries.svelte';
	import type { HeldEntry } from '$lib/features/downloads/queries/types';
	import Button from '$lib/ui/Button.svelte';

	import HeldEntryCard from './HeldEntryCard.svelte';
	import QueueSectionHead from './QueueSectionHead.svelte';

	interface Props {
		/** Files waiting for a decision, one entry per download or upload. */
		entries: HeldEntry[];
		/** The download an entry can hunt again from, when it grabbed the wrong edition. */
		retryTaskOf?: (entry: HeldEntry) => string | null;
	}

	let { entries, retryTaskOf = () => null }: Props = $props();

	const recheckAll = createRecheckAllHeldMutation();
	const recheckEntryIds = $derived(
		entries
			.filter((entry) => entry.files.some((file) => file.actions.includes('recheck')))
			.map((entry) => entry.id)
	);
</script>

<section>
	<QueueSectionHead title="To decide" aside="files kept" count={entries.length}>
		{#snippet actions()}
			{#if recheckEntryIds.length > 0}
				<Button
					variant="outline"
					size="sm"
					icon={RotateCcw}
					loading={recheckAll.isPending}
					onclick={() => recheckAll.mutate(recheckEntryIds)}
					title="Ask AcoustID again. Files it now confirms are imported"
				>
					{recheckAll.isPending ? 'Checking...' : 'Re-check all'}
				</Button>
			{/if}
		{/snippet}
	</QueueSectionHead>
	<div class="space-y-3">
		{#each entries as entry (entry.id)}
			<HeldEntryCard {entry} retryTaskId={retryTaskOf(entry)} />
		{/each}
	</div>
</section>
