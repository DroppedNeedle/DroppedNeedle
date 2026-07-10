<script lang="ts">
	import { Headphones, ArrowRight, Sparkles, AlertTriangle } from 'lucide-svelte';
	import type { ComponentType } from 'svelte';
	import { fromStore } from 'svelte/store';
	import DropImportZone from '$lib/components/import/DropImportZone.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getLocalStatsQuery } from '$lib/queries/local/LocalQueries.svelte';
	import { getLibraryStatsQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import { formatLastUpdated } from '$lib/utils/formatting';
	import type { FormatInfo } from '$lib/types';

	type CardState = 'loading' | 'prompt' | 'error' | 'stats';
	interface Stat {
		value: string;
		label: string;
	}
	interface EntryCard {
		href: string;
		icon: ComponentType;
		title: string;
		subtitle: string;
		state: CardState;
		stats: Stat[];
		footer: string;
		footerWarning: boolean;
		ctaLabel: string;
		promptText: string;
		// written literally so Tailwind keeps the classes
		gradient: string;
		orb: string;
		iconWrap: string;
		ring: string;
		iconColor: string;
		arrow: string;
		cta: string;
	}

	const integrations = fromStore(integrationStore);
	const localEnabled = $derived(integrations.current.localfiles);
	const canImport = $derived(authStore.isTrusted);

	const localStatsQuery = getLocalStatsQuery(() => localEnabled);
	const libraryStatsQuery = getLibraryStatsQuery();

	const localStats = $derived(localStatsQuery.data);
	const libraryStats = $derived(libraryStatsQuery.data);

	function topFormats(breakdown: Record<string, FormatInfo>): string {
		return Object.entries(breakdown)
			.sort((a, b) => b[1].count - a[1].count)
			.slice(0, 2)
			.map(([format]) => format.toUpperCase())
			.join(' · ');
	}
	const localFormats = $derived(localStats ? topFormats(localStats.format_breakdown) : '');

	const libUnmatched = $derived(libraryStats?.unmatched_count ?? 0);
	const libLastScan = $derived(
		libraryStats?.last_scan_at ? new Date(libraryStats.last_scan_at * 1000) : null
	);

	// One card for the whole library: listen, browse, organise (the old Listen +
	// Manage cards merged - owner-signed, phase 01c). Show stats mid-refetch,
	// surface errors instead of an endless skeleton.
	const musicState = $derived<CardState>(
		!integrations.current.loaded
			? 'loading'
			: libraryStats
				? libraryStats.total_albums === 0
					? 'prompt'
					: 'stats'
				: libraryStatsQuery.isError
					? 'error'
					: 'loading'
	);

	const musicFooter = $derived(
		libUnmatched > 0
			? `${libUnmatched} album${libUnmatched === 1 ? '' : 's'} need review`
			: localStats && localStats.total_tracks > 0
				? `${localStats.total_size_human}${localFormats ? ' · ' + localFormats : ''}`
				: libLastScan
					? `Scanned ${formatLastUpdated(libLastScan)}`
					: 'Not scanned yet'
	);

	const musicCard: EntryCard = $derived({
		href: localEnabled ? '/library/local' : '/library',
		icon: Headphones,
		title: 'Your Music',
		subtitle: 'Listen, browse & organise',
		state: musicState,
		stats: libraryStats
			? [
					{ value: libraryStats.total_albums.toLocaleString(), label: 'albums' },
					{ value: libraryStats.total_artists.toLocaleString(), label: 'artists' },
					{ value: libraryStats.total_tracks.toLocaleString(), label: 'tracks' }
				]
			: [],
		footer: musicFooter,
		footerWarning: libUnmatched > 0,
		ctaLabel:
			musicState === 'prompt' ? 'Open library' : localEnabled ? 'Enter the room' : 'Manage library',
		promptText: authStore.isAdmin
			? 'Add a library path and run a scan to fill your library.'
			: 'Your library is being prepared by an admin.',
		gradient: 'from-accent/20 via-secondary/10 to-base-200',
		orb: 'bg-accent/25',
		iconWrap: 'bg-accent/15',
		ring: 'ring-accent/30',
		iconColor: 'text-accent',
		arrow: 'group-hover:text-accent',
		cta: 'text-accent'
	});
</script>

{#snippet entryCard(card: EntryCard)}
	{@const Icon = card.icon}
	<a
		href={card.href}
		class="group card relative overflow-hidden border border-base-content/10 bg-gradient-to-br {card.gradient} shadow-lg transition-all duration-300 hover:-translate-y-1 hover:shadow-2xl focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 focus-visible:outline-none"
	>
		<div
			aria-hidden="true"
			class="pointer-events-none absolute inset-0 opacity-[0.04]"
			style="background-image: url('data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 200 200%22><filter id=%22n%22><feTurbulence type=%22fractalNoise%22 baseFrequency=%220.9%22 numOctaves=%224%22 stitchTiles=%22stitch%22/></filter><rect width=%22100%25%22 height=%22100%25%22 filter=%22url(%23n)%22 opacity=%220.5%22/></svg>'); background-size: 200px;"
		></div>
		<div
			aria-hidden="true"
			class="pointer-events-none absolute -top-12 -right-12 h-44 w-44 rounded-full {card.orb} blur-3xl transition-transform duration-500 group-hover:scale-125"
		></div>

		<div class="card-body relative gap-4 p-5 sm:p-6">
			<div class="flex items-start justify-between gap-3">
				<div class="flex items-center gap-3">
					<div
						class="flex h-12 w-12 shrink-0 items-center justify-center rounded-2xl {card.iconWrap} ring-1 {card.ring} transition-transform duration-300 group-hover:-rotate-6 group-hover:scale-110"
					>
						<Icon class="h-6 w-6 {card.iconColor}" />
					</div>
					<div>
						<h2 class="text-lg font-bold leading-tight sm:text-xl">{card.title}</h2>
						<p class="text-sm text-base-content/60">{card.subtitle}</p>
					</div>
				</div>
				<ArrowRight
					class="h-5 w-5 shrink-0 text-base-content/40 transition-transform duration-300 group-hover:translate-x-1 {card.arrow}"
				/>
			</div>

			{#if card.state === 'loading'}
				<div class="grid grid-cols-3 gap-3">
					{#each Array(3) as _, i (i)}
						<div class="space-y-1.5">
							<div class="skeleton h-7 w-12 rounded"></div>
							<div class="skeleton h-2.5 w-10 rounded"></div>
						</div>
					{/each}
				</div>
				<div class="skeleton mt-auto h-3 w-40 rounded"></div>
			{:else if card.state === 'stats'}
				<div class="grid grid-cols-3 gap-3">
					{#each card.stats as stat (stat.label)}
						<div class="min-w-0">
							<div class="truncate text-2xl font-extrabold tabular-nums sm:text-3xl">
								{stat.value}
							</div>
							<div class="text-[11px] font-medium tracking-wide text-base-content/50 uppercase">
								{stat.label}
							</div>
						</div>
					{/each}
				</div>
				<div
					class="mt-auto flex items-center justify-between gap-2 border-t border-base-content/10 pt-3"
				>
					<span
						class="min-w-0 truncate text-xs {card.footerWarning
							? 'text-warning'
							: 'text-base-content/60'}"
					>
						{card.footer}
					</span>
					<span class="shrink-0 text-xs font-semibold {card.cta}">{card.ctaLabel}</span>
				</div>
			{:else}
				{@const isError = card.state === 'error'}
				<div
					class="flex items-center gap-2 rounded-xl px-4 py-3 text-sm {isError
						? 'bg-warning/10 text-warning'
						: 'bg-base-100/40 text-base-content/70'}"
				>
					{#if isError}
						<AlertTriangle class="h-4 w-4 shrink-0" />
						<span>Couldn't load stats - open to retry.</span>
					{:else}
						<Sparkles class="h-4 w-4 shrink-0 {card.iconColor}" />
						<span>{card.promptText}</span>
					{/if}
				</div>
				<div class="mt-auto flex justify-end border-t border-base-content/10 pt-3">
					<span class="text-xs font-semibold {card.cta}">{card.ctaLabel}</span>
				</div>
			{/if}
		</div>
	</a>
{/snippet}

<div
	class="discover-section-enter grid grid-cols-1 gap-4 sm:gap-5 {canImport ? 'lg:grid-cols-2' : ''}"
>
	{@render entryCard(musicCard)}
	{#if canImport}
		<div class="flex flex-col">
			<DropImportZone className="flex-1 [&>button]:h-full" />
			<a
				href="/downloads?tab=import"
				class="mt-2 self-end text-xs font-semibold text-primary hover:underline"
			>
				View import history
			</a>
		</div>
	{/if}
</div>
