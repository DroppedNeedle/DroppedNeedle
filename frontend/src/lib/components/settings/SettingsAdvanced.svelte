<script lang="ts">
	import { Database, History, ImageIcon } from 'lucide-svelte';
	import {
		createSaveAdvancedSettingsMutation,
		getAdvancedSettingsQuery
	} from '$lib/queries/settings/SectionSettingsQueries.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
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
	let openRetention = $state(false);
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

				<SettingsSectionCollapse
					title="Retention"
					description="How long history and ignored albums are kept"
					icon={History}
					iconBgClass="bg-info/10"
					iconTextClass="text-info"
					bind:isOpen={openRetention}
					name="advanced-settings"
				>
					<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4 pt-2">
						<NumberField
							label="Request History (days)"
							help="Finished requests older than this are removed (default: 180)"
							bind:value={draft.request_history_retention_days}
							min={30}
							max={3650}
							error={section.fieldError('request_history_retention_days')}
						/>
						<NumberField
							label="Ignored Albums (days)"
							help="Ignored albums stay out of the Discover queue this long (default: 365)"
							bind:value={draft.ignored_releases_retention_days}
							min={30}
							max={3650}
							error={section.fieldError('ignored_releases_retention_days')}
						/>
						<NumberField
							label="Clean-up Interval (hours)"
							help="How often old requests and ignored albums are cleared (default: 6)"
							bind:value={draft.store_prune_interval_hours}
							min={1}
							max={168}
							error={section.fieldError('store_prune_interval_hours')}
						/>
					</div>
				</SettingsSectionCollapse>

				<SaveBar {...section.bar} />
			</div>
		{/snippet}
	</QueryBoundary>
</div>
