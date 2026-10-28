<script lang="ts">
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import TimeRangeCard from './TimeRangeCard.svelte';
	import { getTimeRangeFallbackPath } from '$lib/utils/timeRangeFallback';
	import { ChevronLeft, ChevronDown, CircleAlert } from 'lucide-svelte';
	import type { ComponentType } from 'svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import {
		getChartOverviewQuery,
		getChartRangeQuery,
		type ChartItem
	} from '$lib/queries/charts/ChartQueries.svelte';
	import type { ChartKind } from '$lib/queries/charts/endpoints';
	import type { ListenSource, StatsRange } from '$lib/queries/discover/types';

	type ItemType = 'album' | 'artist';

	interface Props {
		itemType: ItemType;
		chart: ChartKind;
		title: string;
		subtitle: string;
		errorIcon?: ComponentType | null;
		source?: ListenSource | null;
	}

	let { itemType, chart, title, subtitle, errorIcon = null, source = null }: Props = $props();

	const timeRanges: { key: StatsRange; label: string }[] = [
		{ key: 'this_week', label: 'This Week' },
		{ key: 'this_month', label: 'This Month' },
		{ key: 'this_year', label: 'This Year' },
		{ key: 'all_time', label: 'All Time' }
	];

	// The expanded range belongs to the source it was opened on, so switching
	// source closes it.
	let expanded = $state<{ source: ListenSource | null; range: StatsRange } | null>(null);
	let expandedRange = $derived(expanded?.source === source ? expanded.range : null);

	const overviewQuery = getChartOverviewQuery(
		() => chart,
		() => source
	);
	const rangeQuery = getChartRangeQuery(
		() => chart,
		() => source,
		() => expandedRange
	);

	let overviewData = $derived(overviewQuery.data ?? null);
	let loading = $derived(overviewQuery.isPending);
	let expandedItems = $derived(rangeQuery.data?.pages.flatMap((page) => page.items) ?? null);
	let loadingMore = $derived(rangeQuery.isFetching);
	let paginationError = $derived(
		rangeQuery.isFetchNextPageError ? `Failed to load more ${itemType}s.` : null
	);

	function expandRange(rangeKey: StatsRange) {
		expanded = expandedRange === rangeKey ? null : { source, range: rangeKey };
	}

	function loadMore() {
		if (rangeQuery.hasNextPage && !rangeQuery.isFetchingNextPage) {
			void rangeQuery.fetchNextPage();
		}
	}

	function loadOverview() {
		void overviewQuery.refetch();
	}

	function getItemHref(item: ChartItem): string | null {
		if (!item.musicbrainz_id) return null;
		if (itemType === 'album') {
			return albumHref(item.musicbrainz_id);
		}
		return artistHref(item.musicbrainz_id);
	}

	function handleItemClick(item: ChartItem) {
		const fallbackPath = getTimeRangeFallbackPath(itemType, item);
		if (fallbackPath) {
			goto(withBasePath(fallbackPath));
		}
	}

	function getItemsForRange(rangeKey: StatsRange): ChartItem[] {
		return overviewData?.[rangeKey]?.items ?? [];
	}

	function getFeaturedForRange(rangeKey: StatsRange): ChartItem | null {
		return overviewData?.[rangeKey]?.featured ?? null;
	}
</script>

<div class="container mx-auto p-4 md:p-6 lg:p-8">
	<div class="mb-6 flex items-center gap-4">
		<IconButton icon={ChevronLeft} label="Back to home" onclick={() => goto(withBasePath('/'))} />
		<div>
			<h1 class="text-3xl font-bold">{title}</h1>
			<p class="mt-1 text-sm text-base-content/70">{subtitle}</p>
		</div>
	</div>

	{#if loading}
		<div class="flex min-h-100 items-center justify-center">
			<span class="loading loading-spinner loading-lg"></span>
		</div>
	{:else if !overviewData}
		<div class="flex min-h-100 flex-col items-center justify-center text-center">
			{#if errorIcon}
				{@const SvelteComponent = errorIcon}
				<SvelteComponent class="h-12 w-12 text-base-content/40 mb-4" strokeWidth={1.5} />
			{:else}
				<CircleAlert class="h-12 w-12 text-base-content/40 mb-4" strokeWidth={1.5} />
			{/if}
			<h2 class="mb-2 text-2xl font-semibold">Unable to load {itemType}s</h2>
			<p class="mb-4 text-base-content/70">Please try again later.</p>
			<Button variant="primary" onclick={loadOverview}>Retry</Button>
		</div>
	{:else}
		<div class="space-y-8">
			{#each timeRanges as range (range.key)}
				{@const featured = getFeaturedForRange(range.key)}
				{@const items = getItemsForRange(range.key)}
				{@const isExpanded = expandedRange === range.key}

				<Card as="section" padding="md">
					<button
						class="mb-2 flex w-full items-center justify-between text-left"
						onclick={() => expandRange(range.key)}
						aria-label="{isExpanded ? 'Collapse' : 'Expand'} {range.label}"
					>
						<h2 class="text-xl font-bold sm:text-2xl">{range.label}</h2>
						<div class="flex items-center gap-2">
							<span class="text-sm text-base-content/50">
								{isExpanded ? 'Show less' : 'View all'}
							</span>
							<ChevronDown class="h-5 w-5 transition-transform {isExpanded ? 'rotate-180' : ''}" />
						</div>
					</button>

					{#if !isExpanded}
						<div class="grid gap-4 lg:grid-cols-3">
							{#if featured}
								{@const featuredHref = getItemHref(featured)}
								<TimeRangeCard
									item={featured}
									{itemType}
									href={featuredHref}
									rank={1}
									variant="featured"
									className="self-start overflow-hidden lg:col-span-1"
									onFallbackClick={handleItemClick}
								/>
							{/if}

							<div class="grid-cards-overview lg:col-span-2">
								{#each items.slice(0, 8) as item, idx (idx)}
									{@const rank = idx + 2}
									{@const itemHref = getItemHref(item)}
									<TimeRangeCard
										{item}
										{itemType}
										href={itemHref}
										{rank}
										variant="overview"
										className=""
										onFallbackClick={handleItemClick}
									/>
								{/each}
							</div>
						</div>
					{:else if loadingMore && !expandedItems}
						<div class="flex justify-center py-8">
							<span class="loading loading-spinner loading-lg"></span>
						</div>
					{:else if expandedItems}
						<div class="grid-cards">
							{#each expandedItems as item, idx (idx)}
								{@const rank = idx + 1}
								{@const itemHref = getItemHref(item)}
								<TimeRangeCard
									{item}
									{itemType}
									href={itemHref}
									{rank}
									variant="expanded"
									className=""
									onFallbackClick={handleItemClick}
								/>
							{/each}
						</div>

						{#if rangeQuery.hasNextPage}
							<div class="mt-6 flex justify-center">
								<Button variant="outline" loading={loadingMore} class="w-64" onclick={loadMore}>
									Load More
								</Button>
							</div>
							{#if paginationError}
								<p class="mt-2 text-center text-sm text-error">{paginationError}</p>
							{/if}
						{/if}
					{/if}
				</Card>
			{/each}
		</div>
	{/if}
</div>
