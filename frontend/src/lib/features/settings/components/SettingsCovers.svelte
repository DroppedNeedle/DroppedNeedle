<script lang="ts">
	import { ImageIcon } from 'lucide-svelte';
	import {
		createSaveAdvancedSettingsMutation,
		getAdvancedSettingsQuery
	} from '$lib/features/settings/queries/SectionSettingsQueries.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const section = createSettingsSection(
		getAdvancedSettingsQuery(),
		createSaveAdvancedSettingsMutation()
	);
</script>

<Card variant="bordered" as="section">
	{#snippet header()}
		<div class="flex items-center gap-2">
			<div class="rounded-lg bg-primary/10 p-2">
				<ImageIcon class="h-5 w-5 text-primary" aria-hidden="true" />
			</div>
			<h3 class="font-semibold">Covers</h3>
		</div>
	{/snippet}
	<QueryBoundary query={section} what="the cover setting">
		{#snippet children(draft)}
			<div class="mt-2 space-y-4">
				<ToggleRow
					label="Prefer local cover art"
					description="Use a cover image in the album folder or the art inside the files before online covers."
					bind:checked={draft.prefer_local_cover_art}
					disabled={section.saving}
				/>
				<SaveBar {...section.bar} />
			</div>
		{/snippet}
	</QueryBoundary>
</Card>
