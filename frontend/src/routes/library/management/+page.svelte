<script lang="ts">
	import {
		ArrowLeft,
		ArrowUpRight,
		FolderCog,
		History,
		LayoutDashboard,
		ScanSearch,
		Settings2
	} from 'lucide-svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { SvelteURL } from 'svelte/reactivity';

	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import LibraryScanningPanel from '$lib/features/library-management/components/LibraryScanningPanel.svelte';
	import LibraryManagementControlRoom from '$lib/features/library-management/components/LibraryManagementControlRoom.svelte';
	import LibraryOverviewPanel from '$lib/features/library-management/components/LibraryOverviewPanel.svelte';
	import SettingsLibraryManagement from '$lib/features/library-management/components/SettingsLibraryManagement.svelte';
	import { getLibraryActivityQuery } from '$lib/features/library-management/queries/LibraryActivityQueries.svelte';
	import { getTargetLibrarySettingsQuery } from '$lib/features/library-management/queries/LibraryPolicyQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	const settingsQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const activityQuery = getLibraryActivityQuery(() => authStore.user?.id);
	const roots = $derived(settingsQuery.data?.library_roots ?? []);
	const libraryEnabled = $derived(settingsQuery.data?.enabled ?? true);
	const policyRevision = $derived(settingsQuery.data?.policy_revision ?? '');
	const workItems = $derived(activityQuery.data?.work_items ?? []);
	const scanWork = $derived(
		workItems.find((item) => item.kind === 'scan' || item.kind === 'identification') ?? null
	);
	const managementWork = $derived(
		workItems.find((item) => item.kind === 'library_management' || item.kind === 'recovery') ?? null
	);
	const scanBadge = $derived.by(() => {
		if (!scanWork) return null;
		if (scanWork.effect === 'attention') return 'Needs attention';
		if (scanWork.remaining_count !== null) return `${scanWork.remaining_count} left`;
		if (scanWork.total && !scanWork.indeterminate) {
			return `${Math.min(100, Math.round((scanWork.processed / scanWork.total) * 100))}%`;
		}
		return scanWork.state === 'queued' ? 'Queued' : 'Running';
	});
	const managementBadge = $derived.by(() => {
		if (!managementWork) return null;
		if (managementWork.effect === 'attention') return 'Needs attention';
		if (managementWork.effect === 'file_writing') return 'Writing';
		return managementWork.state === 'queued' ? 'Queued' : 'Previewing';
	});

	const tabIds = ['overview', 'scanning', 'organize', 'automation'] as const;
	type TabId = (typeof tabIds)[number];

	const requestedTab = $derived(page.url.searchParams.get('tab'));
	// A saved link that opens the runner may name no tab; the runner lives on Organize.
	const fallbackTab = $derived<TabId>(
		page.url.searchParams.has('runner') ? 'organize' : 'overview'
	);
	const activeTab = $derived<TabId>(
		tabIds.includes(requestedTab as TabId) ? (requestedTab as TabId) : fallbackTab
	);

	function selectTab(tab: TabId): void {
		if (tab === activeTab) return;
		const url = new SvelteURL(page.url);
		url.searchParams.set('tab', tab);
		url.hash = '';
		void goto(url, { replaceState: true, noScroll: true, keepFocus: true });
	}

	function tabClass(tab: TabId): string {
		return tab === activeTab ? 'lm-tab lm-tab--on' : 'lm-tab';
	}
</script>

<svelte:head><title>Library Management - DroppedNeedle</title></svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<ListPageHeader
		title="Library Management"
		lede="Scan, identify, and organize your library from one place."
		ticket="DN / CC 3.2"
		fitTitle
	>
		{#snippet actions()}
			<Button variant="outline" icon={ArrowLeft} href={withBasePath('/library')}>
				<span class="hidden sm:inline">Back to Library</span>
				<span class="sm:hidden">Library</span>
			</Button>
		{/snippet}
	</ListPageHeader>

	<main class="space-y-6 px-4 pb-14 sm:px-6 lg:px-8">
		<div class="lm-tabs-scroll">
			<div role="tablist" aria-label="Library Management areas" class="lm-tabs">
				<button
					role="tab"
					id="management-tab-overview"
					aria-controls="management-panel-overview"
					class={tabClass('overview')}
					aria-selected={activeTab === 'overview'}
					onclick={() => selectTab('overview')}
				>
					<LayoutDashboard class="size-5" aria-hidden="true" />
					Overview
				</button>
				<button
					role="tab"
					id="management-tab-scanning"
					aria-controls="management-panel-scanning"
					class={tabClass('scanning')}
					aria-selected={activeTab === 'scanning'}
					onclick={() => selectTab('scanning')}
				>
					<ScanSearch class="size-5" aria-hidden="true" />
					Scanning
					{#if scanBadge}<span class="lm-tab__badge">{scanBadge}</span>{/if}
				</button>
				<button
					role="tab"
					id="management-tab-organize"
					aria-controls="management-panel-organize"
					class={tabClass('organize')}
					aria-selected={activeTab === 'organize'}
					onclick={() => selectTab('organize')}
				>
					<FolderCog class="size-5" aria-hidden="true" />
					Organize files
					{#if managementBadge}<span class="lm-tab__badge">{managementBadge}</span>{/if}
				</button>
				<button
					role="tab"
					id="management-tab-automation"
					aria-controls="management-panel-automation"
					class={tabClass('automation')}
					aria-selected={activeTab === 'automation'}
					onclick={() => selectTab('automation')}
				>
					<Settings2 class="size-5" aria-hidden="true" />
					Automation
				</button>
				<span class="lm-tabs__rule" aria-hidden="true"></span>
				<a role="tab" class="lm-tab" href={withBasePath('/library/management/history')}>
					<History class="size-5" aria-hidden="true" />
					Organization history
					<ArrowUpRight class="size-4" aria-hidden="true" />
				</a>
			</div>
		</div>

		{#if activeTab === 'overview'}
			<div role="tabpanel" id="management-panel-overview" aria-labelledby="management-tab-overview">
				<LibraryOverviewPanel />
			</div>
		{:else if activeTab === 'scanning'}
			<div role="tabpanel" id="management-panel-scanning" aria-labelledby="management-tab-scanning">
				<LibraryScanningPanel />
			</div>
		{:else if activeTab === 'organize'}
			<div role="tabpanel" id="management-panel-organize" aria-labelledby="management-tab-organize">
				{#if settingsQuery.isLoading}
					<div class="skeleton h-64 rounded-box"></div>
				{:else if !libraryEnabled}
					<div class="alert alert-warning">
						<FolderCog class="h-5 w-5" />
						<div class="min-w-0 flex-1">
							<strong>The local library is disabled</strong>
							<p class="text-sm">
								No files are changed while it is off. Turn it on in
								<a class="link link-primary" href={withBasePath('/settings?tab=library')}
									>Settings</a
								> to organize files again.
							</p>
						</div>
					</div>
				{:else}
					<LibraryManagementControlRoom />
				{/if}
			</div>
		{:else}
			<div
				role="tabpanel"
				id="management-panel-automation"
				aria-labelledby="management-tab-automation"
			>
				{#if settingsQuery.isLoading}
					<div class="space-y-3">
						<div class="skeleton h-32 rounded-box"></div>
						<div class="skeleton h-64 rounded-box"></div>
					</div>
				{:else if settingsQuery.isError}
					<div class="alert alert-error">Could not load organization settings.</div>
				{:else if !libraryEnabled}
					<div class="alert alert-warning">
						<Settings2 class="h-5 w-5" />
						<div class="min-w-0 flex-1">
							<strong>The local library is disabled</strong>
							<p class="text-sm">
								Automatic organization is paused. Enable the library in
								<a class="link link-primary" href={withBasePath('/settings?tab=library')}
									>Settings</a
								> to manage profiles and automation again.
							</p>
						</div>
					</div>
				{:else}
					<SettingsLibraryManagement {roots} {policyRevision} />
				{/if}
			</div>
		{/if}
	</main>
</div>

<style>
	/* one bordered bar of condensed caps tabs; on a phone it scrolls sideways */
	.lm-tabs-scroll {
		overflow-x: auto;
		scrollbar-width: none;
	}
	.lm-tabs {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		min-width: max-content;
		padding: 0.375rem;
		border: 1.5px solid var(--dn-edge);
		border-radius: 0.625rem;
		background-color: var(--color-base-100);
	}
	.lm-tab {
		display: inline-flex;
		align-items: center;
		gap: 0.625rem;
		min-height: 2.75rem;
		padding: 0.5rem 1.25rem;
		border: 1.5px solid transparent;
		border-radius: 0.375rem;
		font-family: var(--font-display);
		font-size: 1.125rem;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		font-synthesis: none;
		white-space: nowrap;
		color: var(--color-base-content);
		transition:
			background-color 150ms,
			color 150ms;
	}
	.lm-tab:hover {
		background-color: var(--dn-selected);
	}
	.lm-tab--on,
	.lm-tab--on:hover {
		border-color: var(--dn-action-edge);
		background-color: var(--dn-action);
		color: var(--dn-action-content);
	}
	.lm-tab:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 2px;
	}
	.lm-tab__badge {
		padding: 0.125rem 0.5rem;
		border: 1px solid currentColor;
		border-radius: 9999px;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.08em;
	}
	.lm-tabs__rule {
		flex: none;
		width: 1.5px;
		height: 1.75rem;
		margin-inline: 0.5rem;
		background-color: var(--dn-edge);
	}

	@media (max-width: 767px) {
		.lm-tabs-scroll {
			margin-inline: -1rem;
			padding-inline: 1rem;
		}
		.lm-tab {
			gap: 0.5rem;
			min-height: 2.5rem;
			padding: 0.375rem 0.875rem;
			font-size: 1rem;
		}
		.lm-tab :global(svg) {
			display: none;
		}
	}
</style>
