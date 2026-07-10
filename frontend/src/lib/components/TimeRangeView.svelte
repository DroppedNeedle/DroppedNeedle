<script lang="ts">
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

<div class="container mx-auto px-4 py-6 md:px-6 lg:px-8">
	<header class="mb-8 flex items-start gap-4">
		<button
			class="btn btn-circle btn-ghost border border-base-content/8 bg-base-200/50"
			onclick={() => goto('/')}
			aria-label="Back to home"
		>
			<ChevronLeft class="h-5 w-5" />
		</button>
		<div class="min-w-0">
			<h1 class="font-display text-3xl font-bold tracking-tight sm:text-4xl">{title}</h1>
			<p class="mt-1 text-sm text-base-content/60">{subtitle}</p>
		</div>
	</header>

	{#if loading}
		<div class="flex min-h-100 items-center justify-center">
			<span class="loading loading-spinner loading-lg"></span>
		</div>
	{:else if !overviewData}
		<div class="flex min-h-100 flex-col items-center justify-center text-center">
			{#if errorIcon}
				{@const SvelteComponent = errorIcon}
				<SvelteComponent class="mb-4 h-12 w-12 text-base-content/40" strokeWidth={1.5} />
			{:else}
				<CircleAlert class="mb-4 h-12 w-12 text-base-content/40" strokeWidth={1.5} />
			{/if}
			<h2 class="mb-2 font-display text-2xl font-semibold">Unable to load {itemType}s</h2>
			<p class="mb-4 text-base-content/70">Please try again later.</p>
			<button class="btn btn-primary rounded-full" onclick={loadOverview}>Retry</button>
		</div>
	{:else}
		<div
			class="mb-8 inline-flex flex-wrap items-center gap-1 rounded-full border border-base-content/8 bg-base-200/50 p-1"
			role="group"
			aria-label="Time range"
		>
			{#each timeRanges as range (range.key)}
				<button
					class="rounded-full px-4 py-1.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.16em] transition-colors {expandedRange ===
					range.key
						? 'bg-primary text-primary-content'
						: 'text-base-content/55 hover:text-base-content'}"
					aria-pressed={expandedRange === range.key}
					onclick={() => expandRange(range.key)}
				>
					{range.label}
				</button>
			{/each}
		</div>

		{#if expandedRange}
			{@const activeRange = timeRanges.find((r) => r.key === expandedRange)}
			<section
				class="rounded-2xl border border-base-content/8 bg-base-200/50 p-4 sm:p-6"
				aria-label={activeRange?.label}
			>
				<div class="mb-4 flex items-center justify-between gap-3">
					<h2
						class="font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						{activeRange?.label} — top {itemType}s
					</h2>
					<button
						class="flex items-center gap-1.5 rounded-full border border-base-content/10 px-3 py-1 text-xs text-base-content/55 transition-colors hover:border-base-content/25 hover:text-base-content"
						onclick={() => expandRange(expandedRange!)}
					>
						Show less
						<ChevronDown class="h-3.5 w-3.5 rotate-180" />
					</button>
				</div>

				{#if loadingMore && !expandedData}
					<div class="flex justify-center py-8">
						<span class="loading loading-spinner loading-lg"></span>
					</div>
				{:else if expandedData}
					<div class="flex flex-col divide-y divide-base-content/5">
						{#each expandedData.items as item, idx (idx)}
							{@const rank = idx + 1}
							{@const itemHref = getItemHref(item)}
							<TimeRangeCard
								{item}
								{itemType}
								href={itemHref}
								{rank}
								variant="expanded"
								className="rounded-xl"
								onFallbackClick={handleItemClick}
							/>
						{/each}
					</div>

					{#if expandedData.has_more}
						<div class="mt-6 flex justify-center">
							<button
								class="btn btn-outline btn-wide rounded-full"
								onclick={loadMore}
								disabled={loadingMore}
							>
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
		{:else}
			<div class="space-y-8">
				{#each timeRanges as range (range.key)}
					{@const featured = getFeaturedForRange(range.key)}
					{@const items = getItemsForRange(range.key)}

					<section
						class="rounded-2xl border border-base-content/8 bg-base-200/50 p-4 sm:p-6"
						aria-label={range.label}
					>
						<button
							class="group mb-4 flex w-full items-center justify-between gap-3 text-left"
							onclick={() => expandRange(range.key)}
							aria-label="Expand {range.label}"
						>
							<h2
								class="font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
							>
								{range.label}
							</h2>
							<span
								class="flex items-center gap-1.5 rounded-full border border-base-content/10 px-3 py-1 text-xs text-base-content/55 transition-colors group-hover:text-base-content"
							>
								View all
								<ChevronDown class="h-3.5 w-3.5 -rotate-90" />
							</span>
						</button>

						<div class="grid gap-5 lg:grid-cols-3">
							{#if featured}
								{@const featuredHref = getItemHref(featured)}
								<TimeRangeCard
									item={featured}
									{itemType}
									href={featuredHref}
									rank={1}
									variant="featured"
									className="transition-colors hover:border-base-content/20 lg:col-span-1"
									onFallbackClick={handleItemClick}
								/>
							{/if}

							<div class="flex flex-col divide-y divide-base-content/5 lg:col-span-2">
								{#each items.slice(0, 8) as item, idx (idx)}
									{@const rank = idx + 2}
									{@const itemHref = getItemHref(item)}
									<TimeRangeCard
										{item}
										{itemType}
										href={itemHref}
										{rank}
										variant="overview"
										className="rounded-xl"
										onFallbackClick={handleItemClick}
									/>
								{/each}
							</div>
						</div>
					</section>
				{/each}
			</div>
		{/if}
	{/if}
</div>
