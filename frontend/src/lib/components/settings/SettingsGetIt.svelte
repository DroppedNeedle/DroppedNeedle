<script lang="ts">
	import { Coffee, ExternalLink, ShoppingBag } from 'lucide-svelte';
	import GitHubIcon from '$lib/components/GitHubIcon.svelte';
	import {
		createSaveGetItSettingsMutation,
		getGetItSettingsQuery
	} from '$lib/queries/settings/SectionSettingsQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const section = createSettingsSection(getGetItSettingsQuery(), createSaveGetItSettingsMutation());

	// storefronts the iTunes fallback can target; extend freely
	const regions = [
		{ code: 'US', label: 'United States' },
		{ code: 'GB', label: 'United Kingdom' },
		{ code: 'DE', label: 'Germany' },
		{ code: 'FR', label: 'France' },
		{ code: 'NL', label: 'Netherlands' },
		{ code: 'SE', label: 'Sweden' },
		{ code: 'AU', label: 'Australia' },
		{ code: 'CA', label: 'Canada' },
		{ code: 'JP', label: 'Japan' },
		{ code: 'BR', label: 'Brazil' }
	];
</script>

<Card>
	{#snippet header()}
		<div class="flex items-center gap-2">
			<ShoppingBag class="h-5 w-5 text-primary" aria-hidden="true" />
			<CardTitle>Buy links</CardTitle>
		</div>
	{/snippet}
	<p class="text-sm text-base-content/60">
		Album pages show a "Where to buy" section: Bandcamp first, then other stores, with an iTunes
		fallback when MusicBrainz has no links.
	</p>

	<QueryBoundary query={section} what="the buy link settings">
		{#snippet children(draft)}
			<div class="flex flex-col gap-4 pt-2">
				<div class="max-w-xs">
					<SelectField
						label="Store region"
						bind:value={draft.store_region}
						options={regions.map((region) => ({ value: region.code, label: region.label }))}
						help="The storefront iTunes links point to."
					/>
				</div>
				<SaveBar {...section.bar} />
			</div>
		{/snippet}
	</QueryBoundary>

	<div class="mt-2 border-t border-base-content/10 pt-4">
		<p class="text-sm font-medium">Support development</p>
		<p class="mt-1 text-xs text-base-content/50">
			DroppedNeedle is free to use. If you'd like to help with development, you can donate directly
			through Ko-fi or GitHub Sponsors.
		</p>
		<div class="mt-3 flex flex-wrap gap-2">
			<Button
				variant="outline"
				size="sm"
				icon={Coffee}
				href="https://ko-fi.com/M4M41URGJO"
				target="_blank"
				rel="noopener noreferrer"
			>
				Ko-fi
				<ExternalLink class="h-3.5 w-3.5 opacity-60" aria-hidden="true" />
			</Button>
			<Button
				variant="outline"
				size="sm"
				href="https://github.com/sponsors/HabiRabbu"
				target="_blank"
				rel="noopener noreferrer"
			>
				<GitHubIcon class="h-4 w-4" />
				GitHub Sponsors
				<ExternalLink class="h-3.5 w-3.5 opacity-60" aria-hidden="true" />
			</Button>
		</div>
	</div>
</Card>
