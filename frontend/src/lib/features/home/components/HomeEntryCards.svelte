<script lang="ts">
	import { ArrowRight, Check, ChevronRight, Disc3, Sparkles, TriangleAlert } from 'lucide-svelte';
	import type { ComponentType } from 'svelte';
	import DropImportZone from '$lib/features/downloads/components/DropImportZone.svelte';
	import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getLocalStatsV3Query } from '$lib/features/library/queries/LocalV3Queries.svelte';
	import { formatBytes } from '$lib/utils/formatting';
	import { getLibraryActivityQuery } from '$lib/features/library-management/queries/LibraryActivityQueries.svelte';
	import { getLibraryStatsQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import { formatLastUpdated } from '$lib/utils/formatting';
	import { withBasePath } from '$lib/utils/basePath';
	import Card from '$lib/ui/Card.svelte';

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
		/** The mark before the footer: a warning, a tick for albums to check, or none. */
		footerMark: 'warning' | 'check' | null;
		ctaLabel: string;
		promptText: string;
	}

	const integrations = useIntegrationStatus();
	const localEnabled = $derived(integrations.current.localfiles);
	const canImport = $derived(authStore.isTrusted);

	const localStatsQuery = getLocalStatsV3Query(() => localEnabled);
	const libraryStatsQuery = getLibraryStatsQuery();
	const libraryActivityQuery = getLibraryActivityQuery(() => authStore.user?.id);

	const localStats = $derived(localStatsQuery.data);
	const libraryStats = $derived(libraryStatsQuery.data);
	const identificationActivity = $derived(
		libraryActivityQuery.data?.items.find((item) => item.kind === 'identification')
	);
	const stillMatching = $derived(
		(identificationActivity?.waiting_count ?? 0) + (identificationActivity?.deferred_count ?? 0)
	);
	function topFormats(breakdown: Record<string, number>): string {
		return Object.entries(breakdown)
			.sort((a, b) => b[1] - a[1])
			.slice(0, 2)
			.map(([format]) => format.toUpperCase())
			.join(' · ');
	}
	const localFormats = $derived(localStats ? topFormats(localStats.format_breakdown) : '');

	const libUnmatched = $derived(libraryStats?.review_count ?? 0);
	// Albums to check, each once, as the Check albums badge counts them.
	const libGuesses = $derived(libraryStats?.check_count ?? 0);
	const libLastScan = $derived(
		libraryStats?.last_scan_at ? new Date(libraryStats.last_scan_at * 1000) : null
	);

	// One card for the whole library: listen, browse, organise. Show stats
	// mid-refetch, surface errors instead of an endless skeleton.
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

	const needsDecision = $derived(stillMatching === 0 && libUnmatched > 0);
	const toCheck = $derived(stillMatching === 0 && libUnmatched === 0 && libGuesses > 0);
	const canReview = $derived(
		stillMatching === 0 && (libUnmatched > 0 || libGuesses > 0) && authStore.isTrusted
	);
	const musicFooter = $derived(
		stillMatching > 0 && libUnmatched > 0
			? `${stillMatching.toLocaleString()} still matching, ${libUnmatched.toLocaleString()} need you`
			: stillMatching > 0
				? `${stillMatching.toLocaleString()} still matching`
				: libUnmatched > 0
					? `${libUnmatched.toLocaleString()} need you`
					: libGuesses > 0
						? `${libGuesses.toLocaleString()} to check`
						: localStats && localStats.total_tracks > 0
							? `${formatBytes(localStats.total_size_bytes)}${localFormats ? ' · ' + localFormats : ''}`
							: libLastScan
								? `Scanned ${formatLastUpdated(libLastScan)}`
								: 'Not scanned yet'
	);
	const musicHref = $derived(
		canReview ? '/library/review' : localEnabled ? '/library/local' : '/library'
	);
	const musicCard: EntryCard = $derived({
		href: musicHref,
		icon: Disc3,
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
		footerMark: needsDecision ? 'warning' : toCheck ? 'check' : null,
		ctaLabel:
			musicState === 'prompt'
				? 'Open library'
				: canReview
					? 'Check albums'
					: localEnabled
						? 'Enter the room'
						: 'Manage library',
		promptText: authStore.isAdmin
			? 'Add a library path and run a scan to fill your library.'
			: 'Your library is being prepared by an admin.'
	});
</script>

{#snippet entryCard(card: EntryCard)}
	{@const Icon = card.icon}
	<Card variant="paper" padding="none" href={withBasePath(card.href)} class="group h-full">
		<div class="flex h-full flex-col gap-4 p-5 sm:p-6">
			<div class="flex items-start justify-between gap-3">
				<div class="flex items-center gap-3">
					<span class="music-mark" aria-hidden="true"><Icon class="size-6" /></span>
					<div>
						<h2 class="font-display text-3xl leading-none sm:text-4xl">{card.title}</h2>
						<p class="mt-1 text-sm text-muted">{card.subtitle}</p>
					</div>
				</div>
				<ChevronRight
					class="size-6 shrink-0 transition-transform duration-150 motion-safe:group-hover:translate-x-0.5"
					aria-hidden="true"
				/>
			</div>

			{#if card.state === 'loading'}
				<div class="grid grid-cols-3">
					{#each Array(3) as _, i (i)}
						<div class="stat space-y-1.5">
							<div class="skeleton h-8 w-14 rounded"></div>
							<div class="skeleton h-2.5 w-10 rounded"></div>
						</div>
					{/each}
				</div>
				<div class="skeleton mt-auto h-3 w-40 rounded"></div>
			{:else if card.state === 'stats'}
				<div class="grid grid-cols-3">
					{#each card.stats as stat (stat.label)}
						<div class="stat min-w-0">
							<div class="truncate font-display text-3xl leading-none tabular-nums sm:text-4xl">
								{stat.value}
							</div>
							<div class="mt-1.5 font-mono text-xs tracking-wider text-muted uppercase">
								{stat.label}
							</div>
						</div>
					{/each}
				</div>
				<div
					class="mt-auto flex items-center justify-between gap-3 border-t border-paper-border pt-3"
				>
					<span class="flex min-w-0 items-center gap-2 text-sm font-medium">
						{#if card.footerMark === 'warning'}
							<span class="mark mark--warning" aria-hidden="true">
								<TriangleAlert class="size-3" />
							</span>
						{:else if card.footerMark === 'check'}
							<span class="mark mark--check" aria-hidden="true"><Check class="size-3" /></span>
						{/if}
						<span class="truncate">{card.footer}</span>
					</span>
					<span class="cta">
						{card.ctaLabel}
						<ArrowRight class="size-4" aria-hidden="true" />
					</span>
				</div>
			{:else}
				{@const isError = card.state === 'error'}
				<div
					class="flex items-center gap-2 rounded-lg border border-paper-border px-4 py-3 text-sm"
				>
					{#if isError}
						<span class="mark mark--warning" aria-hidden="true">
							<TriangleAlert class="size-3" />
						</span>
						<span>Couldn't load stats - open to retry.</span>
					{:else}
						<Sparkles class="size-4 shrink-0" aria-hidden="true" />
						<span>{card.promptText}</span>
					{/if}
				</div>
				<div class="mt-auto flex justify-end border-t border-paper-border pt-3">
					<span class="cta">
						{card.ctaLabel}
						<ArrowRight class="size-4" aria-hidden="true" />
					</span>
				</div>
			{/if}
		</div>
	</Card>
{/snippet}

<div
	class="discover-section-enter grid grid-cols-1 gap-4 sm:gap-5 {canImport ? 'lg:grid-cols-2' : ''}"
>
	{@render entryCard(musicCard)}
	{#if canImport}
		<DropImportZone className="h-full">
			{#snippet footer()}
				<a href={withBasePath('/downloads?tab=import')} class="history-link">
					View import history
					<ArrowRight class="size-4" aria-hidden="true" />
				</a>
			{/snippet}
		</DropImportZone>
	{/if}
</div>

<style>
	.music-mark {
		display: grid;
		place-items: center;
		width: 2.75rem;
		height: 2.75rem;
		flex-shrink: 0;
		border-radius: 9999px;
		border: 2px solid var(--dn-paper-content);
		background: var(--dn-action);
		color: var(--dn-action-content);
	}

	/* the three numbers, split by hairlines */
	.stat {
		padding-inline: 1rem;
	}
	.stat:first-child {
		padding-left: 0;
	}
	.stat + .stat {
		border-left: 1px solid var(--dn-paper-border);
	}

	.mark {
		display: grid;
		place-items: center;
		width: 1.25rem;
		height: 1.25rem;
		flex-shrink: 0;
		border-radius: 9999px;
	}
	.mark--check {
		border: 1px solid var(--dn-paper-content);
		background: var(--dn-action);
		color: var(--dn-action-content);
	}
	.mark--warning {
		background: var(--color-warning);
		color: var(--color-warning-content);
	}

	.cta {
		display: inline-flex;
		flex-shrink: 0;
		align-items: center;
		gap: 0.375rem;
		font-size: 0.875rem;
		font-weight: 700;
	}
	.history-link {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		min-height: 2rem;
		border-radius: 0.25rem;
		font-size: 0.875rem;
		font-weight: 700;
		color: var(--dn-title);
	}
	.history-link:hover {
		text-decoration: underline;
		text-underline-offset: 3px;
	}
</style>
