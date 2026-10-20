<script lang="ts">
	import { Database, ImageIcon } from 'lucide-svelte';
	import {
		createSaveAdvancedSettingsMutation,
		getAdvancedSettingsQuery
	} from '$lib/queries/settings/SectionSettingsQueries.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import SettingsSectionCollapse from './SettingsSectionCollapse.svelte';
	import SettingsStorageQueue from './SettingsStorageQueue.svelte';
	import SettingsAudioDB from './SettingsAudioDB.svelte';

	const section = createSettingsSection(
		getAdvancedSettingsQuery(),
		createSaveAdvancedSettingsMutation(),
		{ savedMessage: 'Saved. Some changes apply after a page reload.' }
	);

	let openStorageQueue = $state(true);
	let openAudioDB = $state(false);
</script>

<div class="space-y-6">
	<div>
		<h2 class="text-2xl font-bold">Advanced Settings</h2>
		<p class="text-base-content/60 mt-1">Control the Discover Queue and image loading.</p>
	</div>

	<QueryBoundary query={section} what="the advanced settings">
		{#snippet children(draft)}
			<div class="space-y-6">
				<SettingsSectionCollapse
					title="Discover Queue"
					description="Set how the Discover Queue and Discover Picks are built"
					icon={Database}
					iconBgClass="bg-warning/10"
					iconTextClass="text-warning"
					bind:isOpen={openStorageQueue}
					name="advanced-settings"
				>
					<SettingsStorageQueue bind:data={() => draft, (value) => Object.assign(draft, value)} />
				</SettingsSectionCollapse>

				<SettingsSectionCollapse
					title="AudioDB Images"
					description="Manage artwork from TheAudioDB, including fanart, thumbnails, and logos"
					icon={ImageIcon}
					iconBgClass="bg-primary/10"
					iconTextClass="text-primary"
					bind:isOpen={openAudioDB}
					name="advanced-settings"
				>
					<SettingsAudioDB bind:data={() => draft, (value) => Object.assign(draft, value)} />
				</SettingsSectionCollapse>

				<SaveBar {...section.bar} />
			</div>
		{/snippet}
	</QueryBoundary>
</div>
