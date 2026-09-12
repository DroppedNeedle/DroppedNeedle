<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { Coffee, ExternalLink, ShoppingBag } from 'lucide-svelte';
	import GitHubIcon from '$lib/components/GitHubIcon.svelte';
	import { API } from '$lib/constants';
	import type { GetItSettings } from '$lib/types';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';

	const form = createSettingsForm<GetItSettings>({
		loadEndpoint: API.settingsGetIt(),
		saveEndpoint: API.settingsGetIt()
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

<div class="card bg-base-200">
	<div class="card-body">
		<div class="flex items-center gap-2">
			<ShoppingBag class="h-5 w-5 text-primary" aria-hidden="true" />
			<h2 class="card-title">Buy links</h2>
		</div>
		<p class="text-sm text-base-content/60">
			Album pages show a "Where to buy" section: Bandcamp first, then other stores, with an iTunes
			fallback when MusicBrainz has no links.
		</p>

		{#if form.loading}
			<div class="space-y-3 pt-2">
				<div class="skeleton h-12 w-full rounded-xl"></div>
			</div>
		{:else if form.data}
			<div class="form-control pt-2">
				<label class="label" for="get-it-region">
					<span class="label-text font-medium">Store region</span>
				</label>
				<select
					id="get-it-region"
					class="select select-bordered w-full max-w-xs"
					bind:value={form.data.store_region}
				>
					{#each regions as region (region.code)}
						<option value={region.code}>{region.label}</option>
					{/each}
				</select>
				<p class="mt-1 text-xs text-base-content/50">The storefront iTunes links point to.</p>
			</div>

			<div class="card-actions justify-end pt-2">
				<button class="btn btn-primary btn-sm" onclick={() => form.save()} disabled={form.saving}>
					{form.saving ? 'Saving…' : 'Save'}
				</button>
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
				DroppedNeedle is free to use. If you'd like to help with development, you can donate
				directly through Ko-fi or GitHub Sponsors.
			</p>
			<div class="mt-3 flex flex-wrap gap-2">
				<a
					href="https://ko-fi.com/M4M41URGJO"
					target="_blank"
					rel="noopener noreferrer"
					class="btn btn-ghost btn-sm border border-base-content/15"
				>
					<Coffee class="h-4 w-4" aria-hidden="true" />
					Ko-fi
					<ExternalLink class="h-3.5 w-3.5 opacity-60" aria-hidden="true" />
				</a>
				<a
					href="https://github.com/sponsors/HabiRabbu"
					target="_blank"
					rel="noopener noreferrer"
					class="btn btn-ghost btn-sm border border-base-content/15"
				>
					<GitHubIcon class="h-4 w-4" />
					GitHub Sponsors
					<ExternalLink class="h-3.5 w-3.5 opacity-60" aria-hidden="true" />
				</a>
			</div>
		</div>
	</div>
</div>
