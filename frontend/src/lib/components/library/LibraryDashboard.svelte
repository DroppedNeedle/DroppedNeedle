<script lang="ts">
	import {
		Music,
		Clock,
		AlertTriangle,
		ArrowRight,
		ArrowUp,
		HardDrive,
		Layers
	} from 'lucide-svelte';
	import { onMount } from 'svelte';
	import { fromStore } from 'svelte/store';
	import {
		getLibraryStatsQuery,
		getLibrarySettingsQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import LibraryScanProgress from './LibraryScanProgress.svelte';
	import LibraryScanButton from './LibraryScanButton.svelte';
	import LibraryControlsCard from './LibraryControlsCard.svelte';
	import LibrarySearch from './LibrarySearch.svelte';
	import LocalFilesBand from './LocalFilesBand.svelte';
	import LibraryHubTiles from './LibraryHubTiles.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { formatBytes, formatLastUpdated } from '$lib/utils/formatting';

	const statsQuery = getLibraryStatsQuery();
	const settingsQuery = getLibrarySettingsQuery(() => authStore.isAdmin);

	const integrations = fromStore(integrationStore);
	const localEnabled = $derived(integrations.current.localfiles);
	onMount(() => {
		integrationStore.ensureLoaded();
	});

	const stats = $derived(statsQuery.data);
	const isEmpty = $derived(!!stats && stats.total_albums === 0);
	const hasPath = $derived((settingsQuery.data?.library_paths.length ?? 0) > 0);
	const lastScan = $derived(stats?.last_scan_at ? new Date(stats.last_scan_at * 1000) : null);
	const formatSummary = $derived(
		stats
			? Object.entries(stats.format_breakdown)
					.map(([f, n]) => `${f.toUpperCase()} ${n}`)
					.join(' / ')
			: ''
	);
	const showAttention = $derived(authStore.isAdmin && (stats?.unmatched_count ?? 0) > 0);

	function scrollToTop() {
		window.scrollTo({ top: 0, behavior: 'smooth' });
	}
</script>

<LibraryScanProgress />

{#if statsQuery.isLoading}
	<div class="space-y-4">
		<div class="skeleton h-16 w-full rounded-2xl"></div>
		<div class="grid grid-cols-1 gap-4 sm:grid-cols-3">
			<div class="skeleton h-32 w-full rounded-2xl"></div>
			<div class="skeleton h-32 w-full rounded-2xl"></div>
			<div class="skeleton h-32 w-full rounded-2xl"></div>
		</div>
		<div class="skeleton h-12 w-full rounded-2xl"></div>
	</div>
{:else if statsQuery.isError}
	<div class="alert alert-error rounded-2xl">
		Failed to load library: {statsQuery.error.message}
	</div>
{:else if isEmpty}
	{#if authStore.isAdmin}
		<EmptyState
			icon={Music}
			title="Your library is empty"
			description="Add a library path in Settings and start a scan to get started."
		/>
		<div class="flex justify-center">
			<LibraryScanButton {hasPath} class="btn btn-primary gap-1 rounded-full" />
		</div>
	{:else}
		<EmptyState
			icon={Clock}
			title="Your library is being prepared"
			description="An admin is setting up the library. Check back soon."
		/>
	{/if}
{:else if stats}
	<LibrarySearch />

	<div class="space-y-4">
		<LibraryHubTiles {stats} />

		<div
			class="flex flex-wrap items-center gap-x-6 gap-y-2 rounded-2xl border border-base-content/8 bg-base-200/50 px-5 py-3 text-sm"
		>
			<div class="flex items-center gap-2 text-base-content/55">
				<HardDrive class="h-4 w-4 text-accent/70" />
				<span class="font-mono text-xs font-semibold tabular-nums text-base-content/80">
					{formatBytes(stats.total_size_bytes)}
				</span>
				on disk
			</div>
			<span class="hidden h-4 w-px bg-base-content/10 sm:block"></span>
			<div class="flex items-center gap-2 text-base-content/55">
				<Clock class="h-4 w-4 text-accent/70" />
				Last scan
				<span class="font-mono text-xs font-semibold tabular-nums text-base-content/80">
					{lastScan ? formatLastUpdated(lastScan) : 'never'}
				</span>
			</div>
			<span class="hidden h-4 w-px bg-base-content/10 sm:block"></span>
			<div class="flex min-w-0 items-center gap-2 text-base-content/55">
				<Layers class="h-4 w-4 shrink-0 text-accent/70" />
				<span class="truncate font-mono text-xs font-semibold tabular-nums text-base-content/80">
					{formatSummary || '-'}
				</span>
			</div>
			{#if showAttention}
				<a
					href="/library/unmatched"
					class="ml-auto flex items-center gap-1.5 rounded-full border border-warning/40 bg-warning/10 px-3 py-1 text-xs font-semibold text-warning transition-colors hover:bg-warning/20"
				>
					<AlertTriangle class="h-3.5 w-3.5" />
					{stats.unmatched_count} unmatched
					<ArrowRight class="h-3.5 w-3.5" />
				</a>
			{/if}
		</div>
	</div>

	{#if localEnabled}
		<LocalFilesBand />
	{/if}

	{#if authStore.isAdmin}
		<div id="library-controls" class="scroll-mt-20">
			<LibraryControlsCard {lastScan} />
		</div>
	{/if}

	<div class="flex justify-center pt-4">
		<button
			class="btn btn-ghost btn-sm gap-1.5 rounded-full text-base-content/50 transition-colors hover:text-base-content"
			onclick={scrollToTop}
		>
			<ArrowUp class="h-4 w-4" /> Back to top
		</button>
	</div>
{/if}
