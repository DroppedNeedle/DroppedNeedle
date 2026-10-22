<script lang="ts">
	import { onMount } from 'svelte';
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import SearchSongRow from '$lib/components/SearchSongRow.svelte';
	import {
		SEARCH_BUCKET_PAGE_SIZE,
		getSearchBucketV3Query
	} from '$lib/queries/search/SearchV3Queries.svelte';
	import { toSearchSongs, type SearchSong } from '$lib/queries/search/SearchV3Adapters';
	import { SvelteMap } from 'svelte/reactivity';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
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

	function navigateBack() {
		if (normalizedQuery) {
			goto(withBasePath(`/search?q=${encodeURIComponent(normalizedQuery)}`));
		}
	}

	function navigateToBucket(bucket: 'artists' | 'albums') {
		if (normalizedQuery) {
			goto(withBasePath(`/search/${bucket}?q=${encodeURIComponent(normalizedQuery)}`));
		}
	}

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

<div class="px-4 pt-4 pb-2 sm:px-8">
	<div class="flex gap-2">
		<Button size="sm" pill onclick={navigateBack}>All</Button>
		<Button size="sm" pill onclick={() => navigateToBucket('artists')}>Artists</Button>
		<Button size="sm" pill onclick={() => navigateToBucket('albums')}>Albums</Button>
		<Button variant="primary" size="sm" pill aria-current="page">Songs</Button>
	</div>
</div>
<section class="px-4 py-4 sm:px-8">
	{#if !normalizedQuery}
		<p class="text-center mt-32 text-base-content/50">Enter a search query to get started.</p>
	{:else}
		<QueryBoundary query={firstPage} what="songs">
			{#snippet loading()}
				<div class="overflow-hidden rounded-xl bg-base-100/40 shadow-sm">
					{#each Array(12) as _, i (`loading-song-${i}`)}
						<div class="flex items-center gap-3 px-3 py-2">
							<div class="skeleton h-12 w-12 shrink-0 rounded-md"></div>
							<div class="flex-1 space-y-2">
								<div class="skeleton h-3.5 w-48"></div>
								<div class="skeleton h-3 w-32"></div>
							</div>
						</div>
					{/each}
				</div>
			{/snippet}
			{#snippet empty()}
				<Card><p class="text-center text-base-content/50">No songs found</p></Card>
			{/snippet}
			{#snippet children(loaded)}
				<div
					class="divide-y divide-base-content/5 overflow-hidden rounded-xl bg-base-100/40 shadow-sm"
					aria-label="Song search results"
				>
					{#each loaded as song (song.id)}
						<SearchSongRow {song} />
					{/each}
				</div>

				<div bind:this={sentinel} class="h-20 flex items-center justify-center">
					{#if loading}
						<span class="loading loading-spinner loading-md text-primary"></span>
					{:else if !hasMore}
						<p class="text-base-content/50 text-sm">No more results</p>
					{/if}
				</div>
			{/snippet}
		</QueryBoundary>
	{/if}
</section>
