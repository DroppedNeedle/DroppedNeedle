<script lang="ts">
	import { AlertCircle, CheckCircle, Download, Search } from 'lucide-svelte';

	import EmptyState from '$lib/components/EmptyState.svelte';
	import { getDownloadsQuery } from '$lib/queries/downloads/DownloadQueries.svelte';
	import { getQuarantineQuery } from '$lib/queries/downloads/QuarantineQueries.svelte';
	import {
		bucketDownloads,
		collapseRetryChains,
		nowPressing,
		type DownloadTab
	} from '$lib/queries/downloads/downloadStatus';
	import { authStore } from '$lib/stores/authStore.svelte';

	import DownloadItem from './DownloadItem.svelte';
	import NowPressingHero from './NowPressingHero.svelte';
	import QuarantinePanel from './QuarantinePanel.svelte';

	const query = getDownloadsQuery();
	const isAdmin = $derived(authStore.isAdmin);
	const quarantineQuery = getQuarantineQuery(() => isAdmin);

	let activeTab = $state<DownloadTab>('active');

	// collapse auto-retry chains so each download is one row (the latest attempt), not
	// the original-failed + retry pair
	const tasks = $derived(collapseRetryChains(query.data?.items ?? []));
	const buckets = $derived(bucketDownloads(tasks));
	const hero = $derived(nowPressing(tasks));

	const counts = $derived({
		active: buckets.active.length,
		review: buckets.review.length,
		completed: buckets.completed.length,
		failed: buckets.failed.length,
		quarantine: quarantineQuery.data?.items.length ?? 0
	});

	const tabDefs = $derived<{ key: DownloadTab; label: string }[]>([
		{ key: 'active', label: 'Active' },
		{ key: 'review', label: 'Review' },
		{ key: 'completed', label: 'Completed' },
		{ key: 'failed', label: 'Failed' },
		...(isAdmin ? [{ key: 'quarantine' as DownloadTab, label: 'Quarantine' }] : [])
	]);

	const currentItems = $derived(activeTab === 'quarantine' ? [] : buckets[activeTab]);
</script>

<div class="space-y-4">
	<div
		role="tablist"
		class="flex flex-wrap items-center gap-1 border-b border-base-content/10"
		aria-label="Download queue tabs"
	>
		{#each tabDefs as t (t.key)}
			<button
				role="tab"
				aria-selected={activeTab === t.key}
				class="dl-tab"
				class:dl-tab-active={activeTab === t.key}
				onclick={() => (activeTab = t.key)}
			>
				{t.label}
				<span class="badge badge-ghost badge-sm ml-1 tabular-nums">{counts[t.key]}</span>
			</button>
		{/each}
	</div>

	{#if query.isLoading}
		<div class="space-y-3">
			<div class="skeleton h-20 w-full rounded-2xl"></div>
			<div class="skeleton h-20 w-full rounded-2xl"></div>
			<div class="skeleton h-20 w-full rounded-2xl"></div>
		</div>
	{:else if query.isError}
		<div class="alert alert-error">Couldn't load your downloads - retrying shortly.</div>
	{:else if activeTab === 'quarantine'}
		<QuarantinePanel />
	{:else if currentItems.length === 0}
		{#if activeTab === 'active'}
			<EmptyState
				icon={Download}
				title="No active downloads"
				description="Request an album to get started."
				ctaLabel="Browse Library"
				ctaHref="/library/albums"
			/>
		{:else if activeTab === 'review'}
			<EmptyState
				icon={Search}
				title="Nothing to review"
				description="Downloads that need your review will appear here."
			/>
		{:else if activeTab === 'completed'}
			<EmptyState
				icon={CheckCircle}
				title="No completed downloads"
				description="Completed downloads will appear here."
			/>
		{:else}
			<EmptyState
				icon={AlertCircle}
				title="No failed downloads"
				description="Failed downloads will appear here."
			/>
		{/if}
	{:else}
		{#if activeTab === 'active' && hero}
			<NowPressingHero task={hero} />
		{/if}
		{#key activeTab}
			<div class="stagger-fade-in space-y-3">
				{#each currentItems as task (task.id)}
					{#if !(activeTab === 'active' && hero && task.id === hero.id)}
						<DownloadItem {task} />
					{/if}
				{/each}
			</div>
		{/key}
	{/if}
</div>

<style>
	.dl-tab {
		position: relative;
		padding: 0.5rem 0.85rem;
		font-size: 0.875rem;
		font-weight: 600;
		color: oklch(from var(--color-base-content) l c h / 0.55);
		border-bottom: 2px solid transparent;
		transition:
			color 0.2s ease,
			border-color 0.2s ease;
	}
	.dl-tab:hover {
		color: oklch(from var(--color-base-content) l c h / 0.85);
	}
	.dl-tab-active {
		color: oklch(from var(--color-primary) l c h);
		border-bottom-color: oklch(from var(--color-primary) l c h);
	}
</style>
