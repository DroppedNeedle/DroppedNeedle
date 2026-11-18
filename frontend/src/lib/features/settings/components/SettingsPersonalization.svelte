<script lang="ts">
	import SettingsPaneHeader from './SettingsPaneHeader.svelte';
	import {
		createSavePersonalizationMutation,
		getPersonalizationQuery
	} from '$lib/features/settings/queries/personalization.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const section = createSettingsSection(
		getPersonalizationQuery(),
		createSavePersonalizationMutation()
	);

	let forced = $state(false);

	$effect(() => {
		const draft = section.draft;
		if (draft) forced = draft.mode === 'force';
	});

	$effect(() => {
		const draft = section.draft;
		if (draft) draft.mode = forced ? 'force' : 'allow';
	});
</script>

<div class="flex flex-col gap-5">
	<SettingsPaneHeader
		title="Personalization"
		description="Whether everyone arranges their own Home page, or one layout is served to all."
	/>
	<Card variant="bordered">
		<QueryBoundary query={section} what="the personalization setting">
			{#snippet children(_draft)}
				<div class="mt-2 flex flex-col gap-4">
					<ToggleRow
						label="Use my layout for everyone"
						description="Off, each person arranges their own Home page. On, everyone sees your saved layout and cannot change it; the layouts people saved are kept for when it is switched back off."
						bind:checked={forced}
						disabled={section.saving}
					/>
					<SaveBar {...section.bar} />
				</div>
			{/snippet}
		</QueryBoundary>
	</Card>
</div>
