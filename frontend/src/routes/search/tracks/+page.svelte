<script lang="ts">
	import { onMount } from 'svelte';
	import { browser } from '$app/environment';
	import SearchEmpty from '$lib/features/search/components/SearchEmpty.svelte';
	import SearchHeader from '$lib/features/search/components/SearchHeader.svelte';
	import SearchSongList from '$lib/features/search/components/SearchSongList.svelte';
	import {
		SEARCH_BUCKET_PAGE_SIZE,
		getSearchBucketV3Query
	} from '$lib/features/search/queries/SearchV3Queries.svelte';
	import { toSearchSongs, type SearchSong } from '$lib/features/search/queries/SearchV3Adapters';
	import { Music } from 'lucide-svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';

	interface Props {
		data: { query: string };
	}

	let { data }: Props = $props();

	let normalizedQuery = $derived(data.query.trim());
	let offset = $state(0);
	// Answered pages filed under their echoed offset, so pages land in order
	// even if they resolve out of order.
	let pages = new SvelteMap<number, { items: SearchSong[]; source: unknown }>();
	let sentinel = $state<HTMLElement>();

	const pageQuery = getSearchBucketV3Query(
		() => 'tracks',
		() => normalizedQuery,
		() => SEARCH_BUCKET_PAGE_SIZE,
		() => offset
	);

	// A song can move between pages while they load (a scan renames it):
	// keep its first appearance.
	let songs = $derived.by(() => {
		const seen: Record<string, true> = Object.create(null);
		const out: SearchSong[] = [];
		for (const [, page] of [...pages.entries()].sort(([left], [right]) => left - right)) {
			for (const song of page.items) {
				if (seen[song.id]) continue;
				seen[song.id] = true;
				out.push(song);
			}
		}
		return out;
	});
	let loading = $derived(pageQuery.isPending || pageQuery.isFetching);
	// The boundary answers for the first page: later pages load under the
	// rows already shown, with the spinner below them.
	let firstPage = $derived({
		data: pages.size > 0 ? songs : undefined,
		isPending: pageQuery.isPending,
		isError: pageQuery.isError,
		error: pageQuery.error,
		refetch: resetAndLoad
	});
	let hasMore = $derived.by(() => {
		if (pages.size === 0) return true;
		const lastOffset = Math.max(...pages.keys());
		return (pages.get(lastOffset)?.items.length ?? 0) >= SEARCH_BUCKET_PAGE_SIZE;
	});

	function loadMore() {
		if (loading || !hasMore || !normalizedQuery) return;
		offset += SEARCH_BUCKET_PAGE_SIZE;
	}

	function resetAndLoad() {
		pages.clear();
		if (offset === 0) {
			void pageQuery.refetch();
		} else {
			offset = 0;
		}
	}

	$effect(() => {
		void normalizedQuery;
		offset = 0;
		pages.clear();
	});

	$effect(() => {
		const page = pageQuery.data;
		if (!page || pages.get(page.offset)?.source === page) return;
		pages.set(page.offset, { items: toSearchSongs(page.results), source: page });
	});

	$effect(() => {
		if (!browser || !sentinel) return;
		const current = new IntersectionObserver(
			(entries) => {
				if (entries[0].isIntersecting) loadMore();
			},
			{ threshold: 0.1 }
		);
		current.observe(sentinel);
		return () => current.disconnect();
	});

	onMount(() => {
		if (!browser) return;
		const handleRefresh = () => resetAndLoad();
		window.addEventListener('search-refresh', handleRefresh);
		return () => window.removeEventListener('search-refresh', handleRefresh);
	});
</script>

<SearchHeader query={normalizedQuery} current="tracks" />

<div class="px-4 pt-4 pb-10 sm:px-6 lg:px-8">
	{#if !normalizedQuery}
		<p class="mt-12 text-center text-lg text-muted">Enter a search query to get started.</p>
	{:else}
		<QueryBoundary query={firstPage} what="songs">
			{#snippet loading()}
				<div class="overflow-hidden rounded-box border border-line bg-base-200">
					{#each Array(12) as _, i (`loading-song-${i}`)}
						<div class="flex min-h-16 items-center gap-3.5 px-3.5 py-2">
							<div class="skeleton size-12 shrink-0 rounded-md"></div>
							<div class="flex-1 space-y-2">
								<div class="skeleton h-3.5 w-48"></div>
								<div class="skeleton h-3 w-32"></div>
							</div>
						</div>
					{/each}
				</div>
			{/snippet}
			{#snippet empty()}
				<SearchEmpty icon={Music} title="No songs found" text="Try a different search." />
			{/snippet}
			{#snippet children(loaded)}
				<SearchSongList songs={loaded} />

				<div bind:this={sentinel} class="flex h-20 items-center justify-center">
					{#if loading}
						<span class="loading loading-spinner loading-md text-primary"></span>
					{:else if !hasMore}
						<p class="font-mono text-xs tracking-widest text-muted uppercase">No more results</p>
					{/if}
				</div>
			{/snippet}
		</QueryBoundary>
	{/if}
</div>
