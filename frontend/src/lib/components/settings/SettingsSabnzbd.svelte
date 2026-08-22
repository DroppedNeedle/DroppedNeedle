<script lang="ts">
	import { resolve } from '$app/paths';
	import { CircleCheck, CircleX, FolderTree, Rss, TriangleAlert } from 'lucide-svelte';

	import {
		getSabnzbdConfigQuery,
		saveSabnzbdConfig,
		testSabnzbd
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { getIndexersQuery } from '$lib/queries/downloads/IndexerQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { SabnzbdConnectionSettings, SabnzbdTestResult } from '$lib/types';

	import DownloadClientCard from './DownloadClientCard.svelte';

	const configQuery = getSabnzbdConfigQuery();
	const indexersQuery = getIndexersQuery();
	const save = saveSabnzbdConfig();
	const test = testSabnzbd();

	// SABnzbd only downloads what an indexer finds - a Usenet source with no indexer is inert.
	const hasIndexer = $derived((indexersQuery.data?.length ?? 0) > 0);

	let enabled = $state(false);
	let url = $state('');
	let apiKey = $state('');
	let showKey = $state(false);
	let category = $state('*');
	let downloadsMount = $state('/sabnzbd-downloads');
	let priority = $state(0);
	let postProcessing = $state(3);
	let seeded = $state(false);
	let testResult = $state<SabnzbdTestResult | null>(null);

	$effect(() => {
		const d = configQuery.data;
		if (d && !seeded) {
			enabled = d.enabled;
			url = d.url;
			apiKey = d.api_key;
			category = d.category || '*';
			downloadsMount = d.downloads_mount || '/sabnzbd-downloads';
			priority = d.priority ?? 0;
			postProcessing = d.post_processing ?? 3;
			seeded = true;
		}
	});

	const connected = $derived(testResult?.valid === true);
	const statusText = $derived(
		connected
			? `Connected${testResult?.version ? ` · v${testResult.version}` : ''}`
			: enabled
				? url
					? 'Run Test to check the connection'
					: 'Not configured'
				: 'Disabled'
	);

	function current(): SabnzbdConnectionSettings {
		return {
			enabled,
			client_type: 'sabnzbd',
			url,
			api_key: apiKey,
			category,
			priority,
			post_processing: postProcessing,
			downloads_mount: downloadsMount
		};
	}

	async function onSave() {
		try {
			await save.mutateAsync(current());
			toastStore.show({ message: 'SABnzbd settings saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save SABnzbd settings', type: 'error' });
		}
	}

	// The enable switch sits in the collapsed header, so persist it the moment it flips
	// rather than making the user expand the card and hit Save. Revert on failure.
	async function onToggle() {
		try {
			await save.mutateAsync(current());
			toastStore.show({ message: `SABnzbd ${enabled ? 'enabled' : 'disabled'}`, type: 'success' });
		} catch {
			enabled = !enabled;
			toastStore.show({ message: 'Could not update SABnzbd', type: 'error' });
		}
	}

	async function onTest() {
		try {
			testResult = await test.mutateAsync(current());
		} catch {
			testResult = { valid: false, message: "Couldn't reach SABnzbd", categories: [] };
		}
	}

	const categoryOptions = $derived(
		testResult?.categories?.length
			? testResult.categories
			: [...new Set([category, '*'].filter(Boolean))]
	);
</script>

{#if configQuery.isLoading}
	<div class="skeleton h-28 w-full rounded-box"></div>
{:else if configQuery.isError}
	<div class="alert alert-error">
		Failed to load SABnzbd settings: {configQuery.error.message}
	</div>
{:else}
	<DownloadClientCard
		title="SABnzbd"
		sourceLabel="Usenet"
		icon={Rss}
		{connected}
		{statusText}
		bind:enabled
		{onToggle}
		enableAriaLabel="Enable SABnzbd download client"
	>
		{#if enabled && !indexersQuery.isLoading && !hasIndexer}
			<div class="alert alert-warning items-start text-sm">
				<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
				<div class="space-y-1">
					<p>
						<span class="font-semibold">No indexers configured.</span> SABnzbd downloads the NZBs your
						indexers find - with none set up, Usenet search returns nothing and this client stays idle.
					</p>
					<a class="link link-warning font-medium" href={resolve('/settings?tab=indexers')}>
						Add an indexer →
					</a>
				</div>
			</div>
		{/if}

		<section class="space-y-3">
			<div class="form-control">
				<label class="label" for="sab-url"><span class="label-text">SABnzbd URL</span></label>
				<input
					id="sab-url"
					class="input input-bordered w-full font-mono text-sm"
					bind:value={url}
					placeholder="http://sabnzbd:8080"
				/>
			</div>
			<div class="form-control">
				<label class="label" for="sab-key"><span class="label-text">API key (full)</span></label>
				<div class="join w-full">
					<input
						id="sab-key"
						type={showKey ? 'text' : 'password'}
						class="input input-bordered join-item flex-1 font-mono text-sm"
						bind:value={apiKey}
						placeholder="SABnzbd full API key"
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
							<CircleCheck class="size-4" aria-hidden="true" /> Connected{testResult.version
								? ` · v${testResult.version}`
								: ''}
						{:else}
							<CircleX class="size-4" aria-hidden="true" /> {testResult.message}
						{/if}
					</span>
				{/if}
			</div>
		</section>

		<p class="text-xs leading-relaxed text-base-content/60">
			SABnzbd downloads the NZBs your indexers find. Use the <strong>full</strong> API key (Config → General),
			not the NZB key - the NZB key can't manage the queue.
		</p>

		<div class="form-control">
			<label class="label" for="sab-cat"><span class="label-text">Category</span></label>
			<select id="sab-cat" class="select select-bordered" bind:value={category}>
				{#each categoryOptions as opt (opt)}
					<option value={opt}>{opt}</option>
				{/each}
			</select>
			<span class="label">
				<span class="label-text-alt">
					Run Test to load categories. A dedicated <code>droppedneedle</code> category gives
					predictable folders, but <code>*</code> works.
				</span>
			</span>
		</div>

		<section class="space-y-2">
			<div class="flex items-center gap-2 text-sm font-semibold">
				<FolderTree class="size-4 text-base-content/70" aria-hidden="true" /> Downloads mount
			</div>
			<div class="space-y-1.5 rounded-box border border-base-content/10 bg-base-200/40 p-3">
				<label class="text-sm font-medium" for="sab-mount">Downloads mount</label>
				<p class="text-xs text-base-content/60">
					Where DroppedNeedle sees SABnzbd's completed folder (its <code>complete</code> dir),
					mounted read-write into this container on the same disk as your library.
					{#if testResult?.complete_dir}
						SABnzbd's complete dir is <code class="text-base-content/70"
							>{testResult.complete_dir}</code
						>.
					{/if}
				</p>
				<input
					id="sab-mount"
					type="text"
					class="input input-sm input-bordered w-full font-mono"
					bind:value={downloadsMount}
					placeholder="/sabnzbd-downloads"
				/>
			</div>
		</section>

		<div class="flex justify-end">
			<button class="btn btn-primary" onclick={onSave} disabled={save.isPending}>
				{#if save.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
				Save settings
			</button>
		</div>
	</DownloadClientCard>
{/if}
