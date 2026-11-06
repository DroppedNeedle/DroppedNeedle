<script lang="ts">
	import { onMount } from 'svelte';

	import { Download, PackageOpen } from 'lucide-svelte';

	import DownloadQueue from '$lib/features/downloads/components/DownloadQueue.svelte';
	import DiscoveryBatchList from '$lib/features/discover/components/DiscoveryBatchList.svelte';
	import DropImportJobList from '$lib/features/downloads/components/DropImportJobList.svelte';
	import DropImportZone from '$lib/features/downloads/components/DropImportZone.svelte';
	import ImportFolderLine from '$lib/features/settings/components/ImportFolderLine.svelte';
	import EmptyState from '$lib/ui/EmptyState.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { getIntegrationStatusQuery } from '$lib/features/home/queries/home.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';

	type Tab = 'queue' | 'import';

	const integrationStatus = getIntegrationStatusQuery();

	const isAdmin = $derived(authStore.isAdmin);
	const canImport = $derived(authStore.isTrusted);
	const loaded = $derived(!integrationStatus.isLoading);
	const configured = $derived(integrationStatus.data?.download_client ?? false);

	let activeTab = $state<Tab>('queue');
	let showAllImports = $state(false);

	const tabs: { id: Tab; label: string; icon: typeof Download }[] = [
		{ id: 'queue', label: 'Queue', icon: Download },
		{ id: 'import', label: 'Import', icon: PackageOpen }
	];
	let tabRow = $state<HTMLDivElement | null>(null);

	// Arrow keys, Home and End move between the two tabs, which are one tab stop.
	function moveTab(event: KeyboardEvent) {
		const keys = ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End'];
		if (!keys.includes(event.key)) return;
		event.preventDefault();
		const next: Tab =
			event.key === 'Home'
				? 'queue'
				: event.key === 'End'
					? 'import'
					: activeTab === 'queue'
						? 'import'
						: 'queue';
		activeTab = next;
		tabRow?.querySelector<HTMLElement>(`#downloads-tab-${next}`)?.focus();
	}

	onMount(() => {
		if (new URLSearchParams(window.location.search).get('tab') === 'import') {
			activeTab = 'import';
		}
	});
</script>

<svelte:head>
	<title>Downloads - DroppedNeedle</title>
</svelte:head>

{#snippet tabRowSnippet()}
	<div
		bind:this={tabRow}
		role="tablist"
		aria-label="Downloads"
		tabindex="-1"
		class="dl-tabs"
		onkeydown={moveTab}
	>
		{#each tabs as tab (tab.id)}
			<button
				type="button"
				role="tab"
				id="downloads-tab-{tab.id}"
				class="dl-tab"
				aria-selected={activeTab === tab.id}
				aria-controls="downloads-panel"
				tabindex={activeTab === tab.id ? 0 : -1}
				onclick={() => (activeTab = tab.id)}
			>
				<tab.icon class="size-5" aria-hidden="true" />
				{tab.label}
			</button>
		{/each}
	</div>
{/snippet}

<div class="min-h-[calc(100vh-200px)]">
	<ListPageHeader
		title="Downloads"
		subtitle="The engine room - live transfers, retries, and things needing your call"
		longSubtitle
		snugTitle
		ticket="DN / CC 3.2"
		below={canImport ? tabRowSnippet : undefined}
	/>

	<div
		class="px-4 pb-10 sm:px-6 lg:px-8"
		id="downloads-panel"
		role={canImport ? 'tabpanel' : undefined}
		aria-labelledby={canImport ? `downloads-tab-${activeTab}` : undefined}
	>
		{#if activeTab === 'import' && canImport}
			<div class="space-y-6">
				<DropImportZone />
				<ImportFolderLine />
				{#if isAdmin}
					<div class="dl-everyone">
						<ToggleRow label="Show everyone's imports" bind:checked={showAllImports} />
					</div>
				{/if}
				<DropImportJobList showAll={showAllImports} />
			</div>
		{:else if !loaded}
			<div class="space-y-4" aria-busy="true">
				<div class="skeleton h-5 w-80 max-w-full rounded"></div>
				<div class="skeleton h-40 w-full rounded-box"></div>
				<div class="skeleton h-24 w-full rounded-box"></div>
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
</div>

<style>
	.dl-tabs {
		display: flex;
		gap: 2rem;
		border-bottom: 1px solid var(--dn-line);
	}
	.dl-tab {
		display: inline-flex;
		align-items: center;
		gap: 0.625rem;
		margin-bottom: -1px;
		padding: 0.375rem 0.25rem 0.625rem;
		border-bottom: 3px solid transparent;
		font-family: var(--font-mono);
		font-size: 1.0625rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--dn-muted);
		transition: color 150ms;
	}
	.dl-tab:hover {
		color: var(--color-base-content);
	}
	.dl-tab[aria-selected='true'] {
		color: var(--color-base-content);
		border-bottom-color: var(--dn-title);
	}
	.dl-tab:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 2px;
		border-radius: 0.25rem;
	}
	.dl-everyone {
		max-width: 17rem;
		margin-left: auto;
	}
	@media (max-width: 767px) {
		.dl-tabs {
			gap: 1.25rem;
		}
		.dl-tab {
			font-size: 0.9375rem;
		}
	}
</style>
