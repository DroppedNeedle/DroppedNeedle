<script lang="ts">
	import { onMount } from 'svelte';

	import { Download, PackageOpen } from 'lucide-svelte';

	import DownloadQueue from '$lib/components/downloads/DownloadQueue.svelte';
	import DiscoveryBatchList from '$lib/components/discover/DiscoveryBatchList.svelte';
	import DropImportJobList from '$lib/components/import/DropImportJobList.svelte';
	import DropImportZone from '$lib/components/import/DropImportZone.svelte';
	import PluginSourceSearch from '$lib/components/import/PluginSourceSearch.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { integrationStore } from '$lib/stores/integration';

	const isAdmin = $derived(authStore.isAdmin);
	const canImport = $derived(authStore.isTrusted);
	const loaded = $derived($integrationStore.loaded);
	const configured = $derived($integrationStore.download_client);

	let activeTab = $state<'queue' | 'import'>('queue');
	let showAllImports = $state(false);

	onMount(() => {
		void integrationStore.ensureLoaded();
		// deep link from the Home drop card: /downloads?tab=import
		if (new URLSearchParams(window.location.search).get('tab') === 'import') {
			activeTab = 'import';
		}
	});
</script>

<svelte:head>
	<title>Downloads - DroppedNeedle</title>
</svelte:head>

<div class="mx-auto w-full max-w-5xl px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
	<div class="mb-6">
		<div class="flex items-center gap-2">
			<Download class="h-6 w-6 text-primary" aria-hidden="true" />
			<h1 class="text-2xl font-bold sm:text-3xl">Downloads</h1>
		</div>
		<p class="text-base-content/50 text-sm mt-0.5">
			The engine room - live transfers, retries, and things needing your call
		</p>
	</div>

	{#if canImport}
		<div role="tablist" class="tabs tabs-border mb-6">
			<button
				role="tab"
				class="tab gap-2 {activeTab === 'queue' ? 'tab-active' : ''}"
				aria-selected={activeTab === 'queue'}
				onclick={() => (activeTab = 'queue')}
			>
				<Download class="h-4 w-4" aria-hidden="true" /> Queue
			</button>
			<button
				role="tab"
				class="tab gap-2 {activeTab === 'import' ? 'tab-active' : ''}"
				aria-selected={activeTab === 'import'}
				onclick={() => (activeTab = 'import')}
			>
				<PackageOpen class="h-4 w-4" aria-hidden="true" /> Import
			</button>
		</div>
	{/if}

	{#if activeTab === 'import' && canImport}
		<DropImportZone className="mb-6" />
		<PluginSourceSearch />
		{#if isAdmin}
			<label class="mb-3 flex items-center justify-end gap-2 text-xs text-base-content/60">
				<input type="checkbox" class="toggle toggle-xs" bind:checked={showAllImports} />
				Show everyone's imports
			</label>
		{/if}
		<DropImportJobList showAll={showAllImports} />
	{:else if !loaded}
		<div class="space-y-3">
			<div class="skeleton h-10 w-64 rounded-xl"></div>
			<div class="skeleton h-20 w-full rounded-2xl"></div>
			<div class="skeleton h-20 w-full rounded-2xl"></div>
		</div>
	{:else if !configured}
		{#if isAdmin}
			<EmptyState
				icon={Download}
				title="Download client not configured"
				description="Connect a download client to request albums."
				ctaLabel="Configure Download Client"
				ctaHref="/settings?tab=download-client"
			/>
		{:else}
			<EmptyState
				icon={Download}
				title="Download client not configured"
				description="Contact your admin to configure the download client."
			/>
		{/if}
	{:else}
		<DownloadQueue />
		<DiscoveryBatchList />
	{/if}
</div>
