<script lang="ts">
	import { Music, Clock, ArrowUp, HardDrive, Layers } from 'lucide-svelte';
	import { getLibraryStatsQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import LibrarySearch from './LibrarySearch.svelte';
	import LocalFilesBand from './LocalFilesBand.svelte';
	import LibraryHubTiles from './LibraryHubTiles.svelte';
	import EmptyState from '$lib/ui/EmptyState.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
	import { formatBytes, formatLastUpdated } from '$lib/utils/formatting';

	const statsQuery = getLibraryStatsQuery();

	const integrations = useIntegrationStatus();
	const localEnabled = $derived(integrations.current.localfiles);

	const stats = $derived(statsQuery.data);
	const isEmpty = $derived(!!stats && stats.total_albums === 0);
	const lastScan = $derived(stats?.last_scan_at ? new Date(stats.last_scan_at * 1000) : null);
	const formatSummary = $derived(
		stats
			? Object.entries(stats.format_breakdown)
					.map(([f, n]) => `${f.toUpperCase()} ${n}`)
					.join(' / ')
			: ''
	);
	function scrollToTop() {
		window.scrollTo({ top: 0, behavior: 'smooth' });
	}
</script>

{#if statsQuery.isLoading}
	<div class="space-y-5">
		<div class="skeleton h-[5.25rem] w-full rounded-2xl"></div>
		<div class="grid grid-cols-1 gap-3.5 sm:grid-cols-2 min-[1400px]:grid-cols-4">
			{#each Array(4) as _, i (i)}
				<div class="skeleton h-38 w-full rounded-xl sm:h-59"></div>
			{/each}
		</div>
		<div class="skeleton h-11 w-full rounded-xl"></div>
	</div>
{:else if statsQuery.isError}
	<div class="alert alert-error">Failed to load library: {statsQuery.error.message}</div>
{:else if isEmpty}
	{#if authStore.isAdmin}
		<EmptyState
			icon={Music}
			title="Your library is empty"
			description="Add a library path in Settings, then open Controls to start a scan."
		/>
	{:else}
		<EmptyState
			icon={Clock}
			title="Your library is being prepared"
			description="An admin is setting up the library. Check back soon."
		/>
	{/if}
{:else if stats}
	<LibrarySearch />

	<div class="space-y-6">
		<LibraryHubTiles {stats} />

		<div class="hub-stats">
			<p class="hub-stats__item">
				<HardDrive class="hub-stats__icon" aria-hidden="true" />
				<span><b>{formatBytes(stats.total_size_bytes)}</b> on disk</span>
			</p>
			<span class="hub-stats__rule" aria-hidden="true"></span>
			<p class="hub-stats__item">
				<Clock class="hub-stats__icon" aria-hidden="true" />
				<span>Last scan <b>{lastScan ? formatLastUpdated(lastScan) : 'never'}</b></span>
			</p>
			<span class="hub-stats__rule" aria-hidden="true"></span>
			<p class="hub-stats__item">
				<Layers class="hub-stats__icon" aria-hidden="true" />
				<span>{formatSummary || '-'}</span>
			</p>
		</div>
	</div>

	{#if localEnabled}
		<LocalFilesBand />
	{/if}

	<div class="flex justify-center pt-2">
		<Button variant="caps" size="sm" icon={ArrowUp} onclick={scrollToTop}>Back to top</Button>
	</div>
{/if}

<style>
	.hub-stats {
		display: flex;
		align-items: center;
		gap: 1.5rem;
		min-height: 2.75rem;
		padding: 0.5rem 1.5rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 55%, transparent);
		border-radius: 0.625rem;
		font-family: var(--font-mono);
		font-size: 0.9375rem;
		letter-spacing: 0.1em;
		text-transform: uppercase;
	}
	.hub-stats__item {
		display: flex;
		min-width: 0;
		align-items: center;
		gap: 0.75rem;
	}
	.hub-stats__item b {
		font-weight: 600;
	}
	.hub-stats :global(.hub-stats__icon) {
		width: 1.25rem;
		height: 1.25rem;
		flex: none;
	}
	.hub-stats__rule {
		width: 1px;
		height: 1.25rem;
		margin-inline: 1.5rem;
		background-color: color-mix(in oklab, var(--color-base-content) 55%, transparent);
	}

	/* Phone: three columns in the one bar, the words wrapping in each */
	@media (max-width: 639px) {
		.hub-stats {
			display: grid;
			grid-template-columns: 1fr auto 1fr auto 1.2fr;
			gap: 0.5rem;
			padding: 0.5rem 0.625rem;
			font-size: 0.625rem;
			letter-spacing: 0.06em;
			line-height: 1.3;
		}
		.hub-stats__item {
			gap: 0.5rem;
		}
		.hub-stats :global(.hub-stats__icon) {
			width: 1rem;
			height: 1rem;
		}
		.hub-stats__rule {
			height: 1.75rem;
			margin-inline: 0.25rem;
		}
	}
</style>
