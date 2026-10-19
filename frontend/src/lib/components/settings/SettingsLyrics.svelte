<script lang="ts">
	import { MicVocal } from 'lucide-svelte';
	import {
		createSaveLyricsSettingsMutation,
		getLyricsSettingsQuery
	} from '$lib/queries/settings/LyricsSettingsQueries.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const section = createSettingsSection(
		getLyricsSettingsQuery(),
		createSaveLyricsSettingsMutation()
	);
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
	<QueryBoundary query={section} what="the lyrics setting">
		{#snippet children(draft)}
			<div class="mt-2 space-y-4">
				<ToggleRow
					label="Look up lyrics online"
					description="When lyrics are opened for a track DroppedNeedle has none for, it asks LRCLIB, a free lyrics database."
					bind:checked={draft.enabled}
					disabled={section.saving}
				/>
				<SaveBar {...section.bar} />
			</div>
		{/snippet}
	</QueryBoundary>
</Card>
