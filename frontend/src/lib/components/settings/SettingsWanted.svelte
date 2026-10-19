<script lang="ts">
	import { Radar } from 'lucide-svelte';
	import {
		getWantedSettingsQuery,
		saveWantedSettings
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import Card from '$lib/ui/Card.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const section = createSettingsSection(getWantedSettingsQuery(), saveWantedSettings());
</script>

<Card variant="bordered" as="section">
	{#snippet header()}
		<div class="flex items-center gap-2">
			<div class="rounded-lg bg-secondary/10 p-2">
				<Radar class="h-5 w-5 text-secondary" aria-hidden="true" />
			</div>
			<div>
				<h3 class="font-semibold">Wanted watcher</h3>
				<p class="text-xs text-base-content/50">
					Keeps looking for failed requests. They show under Requests, Wanted.
				</p>
			</div>
		</div>
	{/snippet}
	<QueryBoundary query={section} what="the wanted watcher settings">
		{#snippet children(draft)}
			<div class="mt-2 flex flex-col gap-4">
				<ToggleRow
					label="Watch failed requests"
					description="Off pauses checking. The watchlist is kept."
					bind:checked={draft.enabled}
				/>
				<ToggleRow
					label="Download when a verified copy appears"
					description="Off lists new copies as candidates to review."
					bind:checked={draft.auto_download_on_find}
					disabled={!draft.enabled}
				/>
				<ToggleRow
					label="Also watch albums with missing tracks"
					bind:checked={draft.watch_partial_albums}
					disabled={!draft.enabled}
				/>
				<div class="grid gap-4 sm:grid-cols-2">
					<NumberField
						label="Albums checked per sweep"
						size="sm"
						min={1}
						max={20}
						bind:value={draft.max_checks_per_sweep}
						disabled={!draft.enabled}
					/>
				</div>
				<SaveBar {...section.bar} />
			</div>
		{/snippet}
	</QueryBoundary>
</Card>
