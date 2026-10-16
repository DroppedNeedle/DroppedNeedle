<script lang="ts">
	import { untrack } from 'svelte';
	import { MicVocal } from 'lucide-svelte';
	import {
		createSaveLyricsSettingsMutation,
		getLyricsSettingsQuery
	} from '$lib/queries/settings/LyricsSettingsQueries.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	const query = getLyricsSettingsQuery();
	const save = createSaveLyricsSettingsMutation();

	let enabled = $state(true);
	// The saved value the switch was last filled from.
	let filledFrom: boolean | undefined;
	let status = $state<{ tone: 'success' | 'error'; message: string } | null>(null);

	const saved = $derived(query.data?.enabled);
	const dirty = $derived(saved !== undefined && enabled !== saved);

	// Follow the saved value when it loads or changes on the server, unless
	// there is an unsaved change.
	$effect(() => {
		const latest = saved;
		untrack(() => {
			if (latest !== undefined && (filledFrom === undefined || enabled === filledFrom)) {
				enabled = latest;
			}
			filledFrom = latest;
		});
	});

	function discard() {
		if (saved !== undefined) enabled = saved;
		status = null;
	}

	async function saveSwitch() {
		status = null;
		try {
			const stored = await save.mutateAsync({ enabled });
			enabled = stored.enabled;
			filledFrom = stored.enabled;
			status = { tone: 'success', message: 'Saved.' };
		} catch {
			status = { tone: 'error', message: 'Could not save. Try again.' };
		}
	}
</script>

<Card variant="bordered" as="section">
	{#snippet header()}
		<div class="flex items-center gap-2">
			<div class="rounded-lg bg-secondary/10 p-2">
				<MicVocal class="h-5 w-5 text-secondary" aria-hidden="true" />
			</div>
			<h3 class="font-semibold">Lyrics</h3>
		</div>
	{/snippet}
	<QueryBoundary {query} what="the lyrics setting">
		<div class="mt-2 space-y-4">
			<ToggleRow
				label="Look up lyrics online"
				description="When lyrics are opened for a track DroppedNeedle has none for, it asks LRCLIB, a free lyrics database."
				bind:checked={enabled}
				disabled={save.isPending}
			/>
			<SaveBar onsave={saveSwitch} {dirty} saving={save.isPending} ondiscard={discard} {status} />
		</div>
	</QueryBoundary>
</Card>
