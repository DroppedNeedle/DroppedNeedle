<script lang="ts">
	import SettingsPaneHeader from './SettingsPaneHeader.svelte';
	import {
		createSaveListenBrainzMutation,
		getListenBrainzSettingsQuery
	} from '$lib/features/settings/queries/listenbrainz.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const section = createSettingsSection(
		getListenBrainzSettingsQuery(),
		createSaveListenBrainzMutation()
	);
</script>

<div class="flex flex-col gap-5">
	<SettingsPaneHeader
		title="ListenBrainz"
		description="Recommendations, popular lists and genres from ListenBrainz."
	/>
	<Card variant="bordered">
		<QueryBoundary query={section} what="the ListenBrainz settings">
			{#snippet children(draft)}
				<div class="mt-2 flex flex-col gap-4">
					<ToggleRow
						label="ListenBrainz recommendations"
						description="Scrobbling to each person's own account is not affected."
						bind:checked={draft.reads_enabled}
						disabled={section.saving}
					/>
					<SaveBar {...section.bar} />
				</div>
			{/snippet}
		</QueryBoundary>
	</Card>
</div>
