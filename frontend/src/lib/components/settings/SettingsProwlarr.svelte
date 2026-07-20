<script lang="ts">
	import { CircleCheck, CircleX, Network } from 'lucide-svelte';

	import {
		getProwlarrConfigQuery,
		saveProwlarrConfig,
		testProwlarr
	} from '$lib/queries/downloads/ProwlarrTorrentQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { ProwlarrConnectionSettings, ProwlarrTestResult } from '$lib/types';

	import DownloadClientCard from './DownloadClientCard.svelte';

	const configQuery = getProwlarrConfigQuery();
	const save = saveProwlarrConfig();
	const test = testProwlarr();

	let enabled = $state(false);
	let url = $state('');
	let apiKey = $state('');
	let showKey = $state(false);
	let categoriesText = $state('3000');
	let seeded = $state(false);
	let testResult = $state<ProwlarrTestResult | null>(null);

	$effect(() => {
		const d = configQuery.data;
		if (d && !seeded) {
			enabled = d.enabled;
			url = d.url;
			apiKey = d.api_key;
			categoriesText = d.categories.join(', ') || '3000';
			seeded = true;
		}
	});

	const connected = $derived(testResult?.valid === true);
	const statusText = $derived(
		connected
			? `Connected${testResult?.version ? ` · v${testResult.version}` : ''} · ${testResult?.indexers_total ?? 0} indexer(s)`
			: enabled
				? url
					? 'Run Test to check the connection'
					: 'Not configured'
				: 'Disabled'
	);

	function current(): ProwlarrConnectionSettings {
		const categories = categoriesText
			.split(',')
			.map((value) => Number.parseInt(value.trim(), 10))
			.filter((value) => Number.isInteger(value) && value > 0);
		return { enabled, url, api_key: apiKey, categories: categories.length ? categories : [3000] };
	}

	async function onSave() {
		try {
			await save.mutateAsync(current());
			toastStore.show({ message: 'Prowlarr settings saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save Prowlarr settings', type: 'error' });
		}
	}

	// The enable switch sits in the collapsed header, so persist it the moment it flips.
	async function onToggle() {
		try {
			await save.mutateAsync(current());
			toastStore.show({ message: `Prowlarr ${enabled ? 'enabled' : 'disabled'}`, type: 'success' });
		} catch {
			enabled = !enabled;
			toastStore.show({ message: 'Could not update Prowlarr', type: 'error' });
		}
	}

	async function onTest() {
		try {
			testResult = await test.mutateAsync(current());
		} catch {
			testResult = {
				valid: false,
				message: "Couldn't reach Prowlarr",
				indexers_total: 0,
				indexers_usenet: 0,
				indexers_torrent: 0
			};
		}
	}
</script>

{#if configQuery.isLoading}
	<div class="skeleton h-28 w-full rounded-box"></div>
{:else if configQuery.isError}
	<div class="alert alert-error">
		Failed to load Prowlarr settings: {configQuery.error.message}
	</div>
{:else}
	<DownloadClientCard
		title="Prowlarr"
		sourceLabel="Indexers"
		icon={Network}
		{connected}
		{statusText}
		bind:enabled
		{onToggle}
		enableAriaLabel="Enable Prowlarr indexer connection"
	>
		<section class="space-y-3">
			<div class="form-control">
				<label class="label" for="prowlarr-url"><span class="label-text">Prowlarr URL</span></label>
				<input
					id="prowlarr-url"
					class="input input-bordered w-full font-mono text-sm"
					bind:value={url}
					placeholder="http://prowlarr:9696"
				/>
			</div>
			<div class="form-control">
				<label class="label" for="prowlarr-key"><span class="label-text">API key</span></label>
				<div class="join w-full">
					<input
						id="prowlarr-key"
						type={showKey ? 'text' : 'password'}
						class="input input-bordered join-item flex-1 font-mono text-sm"
						bind:value={apiKey}
						placeholder="Prowlarr API key (Settings → General)"
					/>
					<button
						type="button"
						class="btn join-item"
						onclick={() => (showKey = !showKey)}
						aria-label={showKey ? 'Hide API key' : 'Show API key'}
					>
						{showKey ? 'Hide' : 'Show'}
					</button>
				</div>
			</div>
			<div class="form-control">
				<label class="label" for="prowlarr-categories"
					><span class="label-text">Categories</span></label
				>
				<input
					id="prowlarr-categories"
					class="input input-bordered w-full font-mono text-sm"
					bind:value={categoriesText}
					placeholder="3000, 3010, 3040"
				/>
				<span class="label"
					><span class="label-text-alt"
						>Comma-separated Prowlarr category IDs used for searches.</span
					></span
				>
			</div>
			<div class="flex flex-wrap items-center gap-3">
				<button
					type="button"
					class="btn btn-outline btn-sm"
					onclick={onTest}
					disabled={test.isPending || !url}
				>
					{#if test.isPending}<span class="loading loading-spinner loading-xs"></span>{/if}
					Test connection
				</button>
				{#if testResult}
					<span
						class="flex items-center gap-1.5 text-sm"
						class:text-success={testResult.valid}
						class:text-error={!testResult.valid}
					>
						{#if testResult.valid}
							<CircleCheck class="size-4" aria-hidden="true" />
							{testResult.indexers_total} indexer(s) — {testResult.indexers_usenet} Usenet, {testResult.indexers_torrent}
							torrent
						{:else}
							<CircleX class="size-4" aria-hidden="true" /> {testResult.message}
						{/if}
					</span>
				{/if}
			</div>
		</section>

		<p class="text-xs leading-relaxed text-base-content/60">
			One Prowlarr connection covers <strong>every indexer you manage there</strong> — Usenet and torrent/private
			trackers alike. When enabled, Usenet search goes through Prowlarr instead of the per-indexer list
			below, and torrent search (qBittorrent) becomes available.
		</p>

		<div class="flex justify-end">
			<button class="btn btn-primary" onclick={onSave} disabled={save.isPending}>
				{#if save.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
				Save settings
			</button>
		</div>
	</DownloadClientCard>
{/if}
