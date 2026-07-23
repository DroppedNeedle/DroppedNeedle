<script lang="ts">
	import {
		ArrowLeft,
		FolderCog,
		History,
		ScanSearch,
		Settings2,
		SlidersHorizontal
	} from 'lucide-svelte';

	import PageHeader from '$lib/components/PageHeader.svelte';
	import LibraryOperationsPanel from '$lib/components/library/LibraryOperationsPanel.svelte';
	import SettingsLibraryManagement from '$lib/components/settings/SettingsLibraryManagement.svelte';
	import { getTargetLibrarySettingsQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';

	const settingsQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const roots = $derived(settingsQuery.data?.library_roots ?? []);
	const policyRevision = $derived(settingsQuery.data?.policy_revision ?? '');
</script>

<svelte:head><title>Library Management · DroppedNeedle</title></svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHeader
		subtitle="Administrator controls for catalog scanning and optional, destructive file organisation."
		gradientClass="bg-gradient-to-br from-primary/25 via-base-100 to-warning/15"
	>
		{#snippet title()}Library Management{/snippet}
		{#snippet actions()}
			<a href="/library" class="btn btn-ghost btn-sm gap-2 rounded-full sm:btn-md">
				<ArrowLeft class="h-4 w-4" />
				<span class="hidden sm:inline">Back to Library</span>
				<span class="sm:hidden">Library</span>
			</a>
		{/snippet}
	</PageHeader>

	<main class="space-y-8 px-4 pb-14 sm:px-6 lg:px-8">
		<nav class="library-management-jump-nav" aria-label="Library Management sections">
			<a href="#operations">
				<SlidersHorizontal class="h-4 w-4" />
				<span>Overview</span>
			</a>
			<a href="#scanning-controls" data-tone="scan">
				<ScanSearch class="h-4 w-4" />
				<span>Scan &amp; identify</span>
			</a>
			<a href="#management-controls" data-tone="manage">
				<FolderCog class="h-4 w-4" />
				<span>Manage files</span>
			</a>
			<a href="#management-settings" data-tone="manage">
				<Settings2 class="h-4 w-4" />
				<span>Profiles &amp; automation</span>
			</a>
			<a href="/library/management/history">
				<History class="h-4 w-4" />
				<span>History</span>
			</a>
		</nav>

		<LibraryOperationsPanel />

		<section class="space-y-4" aria-labelledby="management-configuration-title">
			<div>
				<p class="font-mono text-xs uppercase tracking-[0.18em] text-library-manage/80">
					Configuration
				</p>
				<h2 id="management-configuration-title" class="font-display text-2xl font-bold">
					Profiles &amp; automation
				</h2>
				<p class="mt-1 max-w-2xl text-sm text-base-content/55">
					Define exactly what may change, then assign and activate those rules one library root at a
					time.
				</p>
			</div>

			{#if settingsQuery.isLoading}
				<div class="space-y-3">
					<div class="skeleton h-32 rounded-box"></div>
					<div class="skeleton h-64 rounded-box"></div>
				</div>
			{:else if settingsQuery.isError}
				<div class="alert alert-error">Could not load Library Management configuration.</div>
			{:else}
				<SettingsLibraryManagement {roots} {policyRevision} />
			{/if}
		</section>
	</main>
</div>
