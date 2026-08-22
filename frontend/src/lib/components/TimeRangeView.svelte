<script lang="ts">
	import { resolve } from '$app/paths';
	import { run } from 'svelte/legacy';

	import { onDestroy, onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { CACHE_KEYS, CACHE_TTL } from '$lib/constants';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import { createLocalStorageCache } from '$lib/utils/localStorageCache';
	import { overviewCacheSuffix } from '$lib/utils/timeRangeCache';
	import { isAbortError } from '$lib/utils/errorHandling';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { api } from '$lib/api/client';
	import TimeRangeCard from './TimeRangeCard.svelte';
	import { getTimeRangeFallbackPath } from '$lib/utils/timeRangeFallback';
	import type { HomeAlbum, HomeArtist } from '$lib/types';
	import { ChevronLeft, ChevronDown, CircleAlert } from 'lucide-svelte';
	import type { ComponentType } from 'svelte';

	type TimeRangeKey = 'this_week' | 'this_month' | 'this_year' | 'all_time';
	type ItemType = 'album' | 'artist';

	interface TimeRangeData {
		featured: HomeAlbum | HomeArtist | null;
		items: (HomeAlbum | HomeArtist)[];
	}

	interface OverviewData {
		this_week: TimeRangeData;
		this_month: TimeRangeData;
		this_year: TimeRangeData;
		all_time: TimeRangeData;
	}

	interface RangeResponse {
		items: (HomeAlbum | HomeArtist)[];
		offset: number;
		limit: number;
		has_more: boolean;
	}

	interface Props {
		itemType: ItemType;
		endpoint: string;
		title: string;
		subtitle: string;
		errorIcon?: ComponentType | null;
		source?: 'listenbrainz' | 'lastfm' | null;
	}

	let { itemType, endpoint, title, subtitle, errorIcon = null, source = null }: Props = $props();

	const timeRanges: { key: TimeRangeKey; label: string }[] = [
		{ key: 'this_week', label: 'This Week' },
		{ key: 'this_month', label: 'This Month' },
		{ key: 'this_year', label: 'This Year' },
		{ key: 'all_time', label: 'All Time' }
	];

	let overviewData: OverviewData | null = $state(null);
	let expandedRange: TimeRangeKey | null = $state(null);
	let expandedData: RangeResponse | null = $state(null);
	let loading = $state(true);
	let loadingMore = $state(false);
	let paginationError: string | null = $state(null);
	let mounted = $state(false);
	let lastSourceKey = $state('');
	let overviewAbortController: AbortController | null = null;
	let expandAbortController: AbortController | null = null;
	let loadMoreAbortController: AbortController | null = null;

	const overviewCache = createLocalStorageCache<OverviewData>(
		CACHE_KEYS.TIME_RANGE_OVERVIEW_CACHE,
		CACHE_TTL.TIME_RANGE_OVERVIEW,
		{ maxEntries: 40 }
	);

	function getOverviewCacheSuffix(): string {
		// Scope per user; this localStorage cache isn't covered by the TanStack reset, so it could leak across users.
		return overviewCacheSuffix(authStore.user?.id, itemType, source, endpoint);
	}

	function abortInFlightRequests() {
		overviewAbortController?.abort();
		expandAbortController?.abort();
		loadMoreAbortController?.abort();
		overviewAbortController = null;
		expandAbortController = null;
		loadMoreAbortController = null;
	}

	onMount(async () => {
		mounted = true;
		lastSourceKey = source ?? '';
		await loadOverview();
	});

	onDestroy(() => {
		abortInFlightRequests();
	});

	function withSource(url: string): string {
		if (!source) return url;
		const separator = url.includes('?') ? '&' : '?';
		return `${url}${separator}source=${encodeURIComponent(source)}`;
	}

	async function loadOverview() {
		const cacheSuffix = getOverviewCacheSuffix();
		const cachedOverview = overviewCache.get(cacheSuffix);
		const hasCachedOverview = !!cachedOverview?.data;
		const shouldRefresh = !cachedOverview || overviewCache.isStale(cachedOverview.timestamp);

		if (hasCachedOverview) {
			overviewData = cachedOverview.data;
			loading = false;
		}

		if (!shouldRefresh) {
			return;
		}

		if (!hasCachedOverview) {
			loading = true;
		}

		overviewAbortController?.abort();
		const controller = new AbortController();
		overviewAbortController = controller;

		try {
			const data = await api.get<OverviewData>(withSource(`${endpoint}?limit=10`), {
				signal: controller.signal
			});
			if (controller.signal.aborted) {
				return;
			}
			overviewData = data;
			overviewCache.set(data, cacheSuffix);
		} catch (error) {
			if (isAbortError(error)) {
				return;
			}
		} finally {
			if (!controller.signal.aborted) {
				loading = false;
			}
			if (overviewAbortController === controller) {
				overviewAbortController = null;
			}
		}
	}

	async function expandRange(rangeKey: TimeRangeKey) {
		if (expandedRange === rangeKey) {
			expandedRange = null;
			expandedData = null;
			paginationError = null;
			return;
		}

		expandedRange = rangeKey;
		paginationError = null;
		loadingMore = true;
		expandAbortController?.abort();
		const controller = new AbortController();
		expandAbortController = controller;
		try {
			const data = await api.get<RangeResponse>(
				withSource(`${endpoint}/${rangeKey}?limit=25&offset=0`),
				{
					signal: controller.signal
				}
			);
			if (controller.signal.aborted) {
				return;
			}
			expandedData = data;
		} catch (error) {
			if (isAbortError(error)) {
				return;
			}
		} finally {
			if (!controller.signal.aborted) {
				loadingMore = false;
			}
			if (expandAbortController === controller) {
				expandAbortController = null;
			}
		}
	}

	async function loadMore() {
		if (!expandedRange || !expandedData || loadingMore || !expandedData.has_more) return;

		loadingMore = true;
		paginationError = null;
		loadMoreAbortController?.abort();
		const controller = new AbortController();
		loadMoreAbortController = controller;
		try {
			const newOffset = expandedData.offset + expandedData.limit;
			const moreData = await api.get<RangeResponse>(
				withSource(`${endpoint}/${expandedRange}?limit=25&offset=${newOffset}`),
				{
					signal: controller.signal
				}
			);
			if (controller.signal.aborted) {
				return;
			}
			expandedData = {
				...moreData,
				items: [...expandedData.items, ...moreData.items]
			};
		} catch (error) {
			if (isAbortError(error)) {
				return;
			}
			paginationError = `Failed to load more ${itemType}s.`;
		} finally {
			if (!controller.signal.aborted) {
				loadingMore = false;
			}
			if (loadMoreAbortController === controller) {
				loadMoreAbortController = null;
			}
		}
	}

	function getItemHref(item: HomeAlbum | HomeArtist): string | null {
		if (!item.mbid) return null;
		if (itemType === 'album') {
			return albumHref(item.mbid);
		}
		return artistHref(item.mbid);
	}

	function handleItemClick(item: HomeAlbum | HomeArtist) {
		const fallbackPath = getFallbackSearchPath(item);
		if (fallbackPath) {
			goto(fallbackPath);
		}
	}

	function getFallbackSearchPath(item: HomeAlbum | HomeArtist): string | null {
		return getTimeRangeFallbackPath(itemType, item);
	}

	function getItemsForRange(rangeKey: TimeRangeKey): (HomeAlbum | HomeArtist)[] {
		if (!overviewData) return [];
		return overviewData[rangeKey]?.items || [];
	}

	function getFeaturedForRange(rangeKey: TimeRangeKey): HomeAlbum | HomeArtist | null {
		if (!overviewData) return null;
		return overviewData[rangeKey]?.featured || null;
	}
	run(() => {
		if (mounted && (source ?? '') !== lastSourceKey) {
			abortInFlightRequests();
			lastSourceKey = source ?? '';
			expandedRange = null;
			expandedData = null;
			loadOverview();
		}
	});
</script>

<div class="container mx-auto p-4 md:p-6 lg:p-8">
	<div class="mb-6 flex items-center gap-4">
		<button
			class="btn btn-circle btn-ghost"
			onclick={() => goto(resolve('/'))}
			aria-label="Back to home"
		>
			<ChevronLeft class="h-6 w-6" />
		</button>
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
			<button class="btn btn-primary" onclick={loadOverview}>Retry</button>
		</div>
	{:else}
		<div class="space-y-8">
			{#each timeRanges as range (range.key)}
				{@const featured = getFeaturedForRange(range.key)}
				{@const items = getItemsForRange(range.key)}
				{@const isExpanded = expandedRange === range.key}

				<section class="rounded-2xl bg-base-200/50 p-4 sm:p-6">
					<button
						class="mb-4 flex w-full items-center justify-between text-left"
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
									className="overflow-hidden bg-base-100 shadow-lg transition-all hover:shadow-xl lg:col-span-1"
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
										className="bg-base-100 shadow-sm transition-all hover:scale-105 hover:shadow-lg active:scale-95"
										onFallbackClick={handleItemClick}
									/>
								{/each}
							</div>
						</div>
					{:else if loadingMore && !expandedData}
						<div class="flex justify-center py-8">
							<span class="loading loading-spinner loading-lg"></span>
						</div>
					{:else if expandedData}
						<div class="grid-cards">
							{#each expandedData.items as item, idx (idx)}
								{@const rank = idx + 1}
								{@const itemHref = getItemHref(item)}
								<TimeRangeCard
									{item}
									{itemType}
									href={itemHref}
									{rank}
									variant="expanded"
									className="bg-base-100 shadow-sm transition-all hover:scale-105 hover:shadow-lg active:scale-95"
									onFallbackClick={handleItemClick}
								/>
							{/each}
						</div>

						{#if expandedData.has_more}
							<div class="mt-6 flex justify-center">
								<button class="btn btn-outline btn-wide" onclick={loadMore} disabled={loadingMore}>
									{#if loadingMore}
										<span class="loading loading-spinner loading-sm"></span>
									{:else}
										Load More
									{/if}
								</button>
							</div>
							{#if paginationError}
								<p class="mt-2 text-center text-sm text-error">{paginationError}</p>
							{/if}
						{/if}
					{/if}
				</section>
			{/each}
		</div>
	{/if}
</div>
