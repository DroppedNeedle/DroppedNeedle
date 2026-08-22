<script lang="ts">
	import { resolve } from '$app/paths';
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
	import { onMount, tick } from 'svelte';

	import PageHeader from '$lib/components/PageHeader.svelte';
	import LibraryScanningPanel from '$lib/components/library/LibraryScanningPanel.svelte';
	import LibraryManagementControlRoom from '$lib/components/library/LibraryManagementControlRoom.svelte';
	import LibraryOverviewPanel from '$lib/components/library/LibraryOverviewPanel.svelte';
	import SettingsLibraryManagement from '$lib/components/settings/SettingsLibraryManagement.svelte';
	import { getLibraryActivityQuery } from '$lib/queries/library/LibraryActivityQueries.svelte';
	import { getTargetLibrarySettingsQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';

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
	const activeTab = $derived<TabId>(
		tabIds.includes(requestedTab as TabId) ? (requestedTab as TabId) : 'overview'
	);

	function selectTab(tab: TabId): void {
		if (tab === activeTab) return;
		const url = new SvelteURL(page.url);
		url.searchParams.set('tab', tab);
		url.hash = '';
		void goto(url, { replaceState: true, noScroll: true, keepFocus: true });
	}

	const segmentBase =
		'flex items-center gap-2 rounded-xl px-4 py-2.5 text-sm font-semibold transition-colors';
	const segmentIdle = `${segmentBase} text-base-content/60 hover:bg-base-100/70 hover:text-base-content`;

	function segmentClass(tab: TabId): string {
		if (tab !== activeTab) return segmentIdle;
		if (tab === 'scanning') return `${segmentBase} bg-primary/15 text-primary glow-primary-soft`;
		if (tab === 'organize') return `${segmentBase} bg-warning/15 text-warning`;
		return `${segmentBase} bg-base-100 text-base-content shadow-sm`;
	}

	function legacyTarget(): TabId | null {
		const hash = page.url.hash;
		const runner = page.url.searchParams.get('runner');
		if (hash === '#scanning-controls' || hash === '#recent-runs') return 'scanning';
		if (
			hash === '#management-controls' ||
			hash === '#identity-readiness' ||
			runner === 'manage' ||
			runner === 'baseline_restore'
		)
			return 'organize';
		if (hash === '#management-settings') return 'automation';
		return null;
	}

	async function applyLegacyLink(): Promise<void> {
		const target = legacyTarget();
		if (!target) return;
		const anchorId = page.url.hash.startsWith('#') ? page.url.hash.slice(1) : null;
		if (target !== activeTab) {
			const url = new SvelteURL(page.url);
			url.searchParams.set('tab', target);
			url.hash = '';
			await goto(url, { replaceState: true, noScroll: true, keepFocus: true });
		}
		if (!anchorId) return;
		await tick();
		const anchor = document.getElementById(anchorId);
		if (!(anchor instanceof HTMLElement)) return;
		anchor.scrollIntoView({
			behavior: window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth',
			block: 'start'
		});
	}

	onMount(() => {
		void applyLegacyLink();
		const handleHashChange = (): void => void applyLegacyLink();
		window.addEventListener('hashchange', handleHashChange);
		return () => window.removeEventListener('hashchange', handleHashChange);
	});
</script>

<svelte:head><title>Library Management · DroppedNeedle</title></svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHeader
		subtitle="Scan, identify, and organize your library from one place."
		gradientClass="bg-gradient-to-br from-primary/25 via-base-100 to-warning/15"
	>
		{#snippet title()}Library Management{/snippet}
		{#snippet actions()}
			<a href={resolve('/library')} class="btn btn-ghost btn-sm gap-2 rounded-full sm:btn-md">
				<ArrowLeft class="h-4 w-4" />
				<span class="hidden sm:inline">Back to Library</span>
				<span class="sm:hidden">Library</span>
			</a>
		{/snippet}
	</PageHeader>

	<main class="space-y-6 px-4 pb-14 sm:px-6 lg:px-8">
		<div
			role="tablist"
			aria-label="Library Management areas"
			class="flex flex-wrap gap-1 rounded-2xl border border-base-content/10 bg-base-200/50 p-1.5"
		>
			<button
				role="tab"
				id="management-tab-overview"
				aria-controls="management-panel-overview"
				class={segmentClass('overview')}
				aria-selected={activeTab === 'overview'}
				onclick={() => selectTab('overview')}
			>
				<LayoutDashboard class="h-4 w-4" />
				Overview
			</button>
			<button
				role="tab"
				id="management-tab-scanning"
				aria-controls="management-panel-scanning"
				class={segmentClass('scanning')}
				aria-selected={activeTab === 'scanning'}
				onclick={() => selectTab('scanning')}
			>
				<ScanSearch class="h-4 w-4" />
				Scanning
				{#if scanBadge}<span class="badge badge-sm">{scanBadge}</span>{/if}
			</button>
			<button
				role="tab"
				id="management-tab-organize"
				aria-controls="management-panel-organize"
				class={segmentClass('organize')}
				aria-selected={activeTab === 'organize'}
				onclick={() => selectTab('organize')}
			>
				<FolderCog class="h-4 w-4" />
				Organize files
				{#if managementBadge}<span class="badge badge-sm">{managementBadge}</span>{/if}
			</button>
			<button
				role="tab"
				id="management-tab-automation"
				aria-controls="management-panel-automation"
				class={segmentClass('automation')}
				aria-selected={activeTab === 'automation'}
				onclick={() => selectTab('automation')}
			>
				<Settings2 class="h-4 w-4" />
				Automation
			</button>
			<a role="tab" class={segmentIdle} href={resolve('/library/management/history')}>
				<History class="h-4 w-4" />
				Organization history
				<ArrowUpRight class="h-3.5 w-3.5 opacity-60" />
			</a>
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
				{#if !libraryEnabled}
					<div class="alert alert-warning">
						<FolderCog class="h-5 w-5" />
						<div class="min-w-0 flex-1">
							<strong>The local library is disabled</strong>
							<p class="text-sm">
								File organization is paused. Existing catalog data and playback keep working. Enable
								the library in
								<a class="link link-primary" href={resolve('/settings?tab=library')}>Settings</a> to run
								organization again.
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
								<a class="link link-primary" href={resolve('/settings?tab=library')}>Settings</a> to manage
								profiles and automation again.
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
