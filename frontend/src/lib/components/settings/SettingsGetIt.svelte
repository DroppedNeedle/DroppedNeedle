<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { onDestroy, onMount } from 'svelte';
	import { Coffee, ExternalLink, ShoppingBag } from 'lucide-svelte';
	import GitHubIcon from '$lib/components/GitHubIcon.svelte';
	import type { GetItSettings } from '$lib/types';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';

	const form = createSettingsForm<GetItSettings>({
		loadEndpoint: SETTINGS_ENDPOINTS.getIt(),
		saveEndpoint: SETTINGS_ENDPOINTS.getIt()
	});

	onMount(() => form.load());
	onDestroy(() => form.cleanup());

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
			<h2 class="card-title">Buy links</h2>
		</div>
	{/snippet}
	<p class="text-sm text-base-content/60">
		Album pages show a "Where to buy" section: Bandcamp first, then other stores, with an iTunes
		fallback when MusicBrainz has no links.
	</p>

	{#if form.loading}
		<div class="space-y-3 pt-2">
			<div class="skeleton h-12 w-full rounded-xl"></div>
		</div>
	{:else if form.data}
		<div class="max-w-xs pt-2">
			<SelectField
				label="Store region"
				bind:value={form.data.store_region}
				options={regions.map((region) => ({ value: region.code, label: region.label }))}
				help="The storefront iTunes links point to."
			/>
		</div>

		<div class="flex justify-end pt-2">
			<Button variant="primary" size="sm" onclick={() => form.save()} disabled={form.saving}>
				{form.saving ? 'Saving…' : 'Save'}
			</Button>
		</div>

		{#if form.message}
			<p class="text-sm {form.messageType === 'error' ? 'text-error' : 'text-success'}">
				{form.message}
			</p>
		{/if}
	{/if}

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
