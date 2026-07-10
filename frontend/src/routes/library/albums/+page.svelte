<script lang="ts">
	/*
	 * Vinyl Collection — the record wall. A filter rail on the left (view mode,
	 * mastering format, sort) and a large-type collection header over the wall.
	 * Browse state stays in the URL (page / sort / q / format) so links remain
	 * shareable and the back button stays honest — same contract as before.
	 */
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import {
		ChevronLeft,
		Disc3,
		Search,
		X,
		LayoutGrid,
		List,
		ArrowDownWideNarrow
	} from 'lucide-svelte';
	import { getLibraryAlbumsQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import LibraryAlbumCard from '$lib/components/library/LibraryAlbumCard.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { formatBytes } from '$lib/utils/formatting';
	import type { AlbumSort } from '$lib/types';

	const PAGE_SIZE = 50;
	const SEARCH_DEBOUNCE_MS = 300;
	const VALID_SORTS: AlbumSort[] = ['recent', 'title', 'artist'];
	const SORT_LABELS: Record<AlbumSort, string> = {
		recent: 'Date added',
		title: 'Title A–Z',
		artist: 'Artist A–Z'
	};
	const FORMATS = ['flac', 'mp3', 'm4a', 'opus', 'ogg'];
	const VIEW_KEY = 'dn-collection-view';

	// browse state lives in the url so it's shareable and back-button stable
	const params = $derived.by(() => {
		const sp = page.url.searchParams;
		const pageNum = Math.max(1, parseInt(sp.get('page') ?? '1', 10) || 1);
		const rawSort = (sp.get('sort') ?? 'recent') as AlbumSort;
		return {
			page: pageNum,
			sort: VALID_SORTS.includes(rawSort) ? rawSort : 'recent',
			q: sp.get('q') ?? '',
			format: sp.get('format') ?? ''
		};
	});

	const albumsQuery = getLibraryAlbumsQuery(() => params);
	const total = $derived(albumsQuery.data?.total ?? 0);
	const totalPages = $derived(
		albumsQuery.data ? Math.max(1, Math.ceil(albumsQuery.data.total / PAGE_SIZE)) : 1
	);

	// view mode is a device preference, not shareable state
	let viewMode = $state<'wall' | 'list'>('wall');
	$effect(() => {
		try {
			const stored = localStorage.getItem(VIEW_KEY);
			if (stored === 'list' || stored === 'wall') viewMode = stored;
		} catch {
			/* private mode */
		}
	});
	function setViewMode(mode: 'wall' | 'list') {
		viewMode = mode;
		try {
			localStorage.setItem(VIEW_KEY, mode);
		} catch {
			/* private mode */
		}
	}

	// local edits win over the url term until back/forward navigation re-syncs the input
	let searchInput = $derived(params.q);
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;
	$effect(() => () => clearTimeout(searchTimeout));

	function setParams(updates: Record<string, string | number | null>) {
		const url = new URL(page.url);
		for (const [k, v] of Object.entries(updates)) {
			if (v === null || v === '') url.searchParams.delete(k);
			else url.searchParams.set(k, String(v));
		}
		goto(url, { replaceState: true, keepFocus: true, noScroll: true });
	}

	function handleSearchInput(e: Event) {
		searchInput = (e.target as HTMLInputElement).value;
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(
			() => setParams({ q: searchInput.trim(), page: null }),
			SEARCH_DEBOUNCE_MS
		);
	}

	function clearSearch() {
		searchInput = '';
		clearTimeout(searchTimeout);
		setParams({ q: null, page: null });
	}
</script>

<svelte:head><title>Vinyl Collection · DroppedNeedle</title></svelte:head>

<div class="mx-auto max-w-screen-2xl px-4 py-6 sm:px-6 lg:px-8">
	<div class="flex flex-col gap-8 lg:flex-row">
		<!-- ═══ filter rail ═══ -->
		<aside
			class="w-full shrink-0 lg:sticky lg:top-20 lg:max-h-[calc(100vh-6rem)] lg:w-56 lg:self-start lg:overflow-y-auto"
			aria-label="Collection filters"
		>
			<div class="mb-5 flex items-center justify-between">
				<h2 class="font-display text-lg font-bold">Filters</h2>
				<button
					class="btn btn-ghost btn-xs btn-circle"
					onclick={() => goto('/library')}
					aria-label="Back to library"
					title="Back to library"
				>
					<ChevronLeft class="h-4 w-4" />
				</button>
			</div>

			<div class="space-y-6">
				<div>
					<p class="dn-filter-label">View mode</p>
					<div class="join w-full">
						<button
							class="btn btn-sm join-item flex-1 gap-1.5 {viewMode === 'wall'
								? 'btn-primary'
								: 'btn-ghost bg-base-content/6'}"
							onclick={() => setViewMode('wall')}
							aria-pressed={viewMode === 'wall'}
						>
							<LayoutGrid class="h-3.5 w-3.5" />
							Wall
						</button>
						<button
							class="btn btn-sm join-item flex-1 gap-1.5 {viewMode === 'list'
								? 'btn-primary'
								: 'btn-ghost bg-base-content/6'}"
							onclick={() => setViewMode('list')}
							aria-pressed={viewMode === 'list'}
						>
							<List class="h-3.5 w-3.5" />
							List
						</button>
					</div>
				</div>

				<div>
					<p class="dn-filter-label">Mastering format</p>
					<div class="flex flex-wrap gap-1.5">
						<button
							class="btn btn-xs rounded-full font-mono uppercase tracking-wider {params.format ===
							''
								? 'btn-primary'
								: 'btn-ghost bg-base-content/6'}"
							onclick={() => setParams({ format: null, page: null })}
						>
							All
						</button>
						{#each FORMATS as f (f)}
							<button
								class="btn btn-xs rounded-full font-mono uppercase tracking-wider {params.format ===
								f
									? 'btn-primary'
									: 'btn-ghost bg-base-content/6'}"
								onclick={() => setParams({ format: f, page: null })}
								aria-pressed={params.format === f}
							>
								{f}
							</button>
						{/each}
					</div>
				</div>

				<div>
					<p class="dn-filter-label">Sort</p>
					<div class="flex flex-col gap-1">
						{#each VALID_SORTS as sort (sort)}
							<button
								class="btn btn-sm justify-start gap-2 {params.sort === sort
									? 'btn-primary'
									: 'btn-ghost bg-base-content/4'}"
								onclick={() => setParams({ sort, page: null })}
								aria-pressed={params.sort === sort}
							>
								<ArrowDownWideNarrow class="h-3.5 w-3.5 opacity-60" />
								{SORT_LABELS[sort]}
							</button>
						{/each}
					</div>
				</div>
			</div>
		</aside>

		<!-- ═══ the wall ═══ -->
		<main class="min-w-0 flex-1">
			<header class="mb-6">
				<h1 class="hero-title font-display text-4xl font-bold tracking-tight sm:text-5xl">
					Vinyl Collection
				</h1>
				<p class="mt-2 text-sm text-base-content/55">
					{total}
					{total === 1 ? 'record' : 'records'} curated in your vault
				</p>
			</header>

			<div class="relative mb-6 max-w-xl">
				<Search
					class="pointer-events-none absolute left-4 top-1/2 h-4 w-4 -translate-y-1/2 text-base-content/40"
				/>
				<input
					type="text"
					placeholder="Search the collection…"
					class="input input-bordered w-full rounded-full pl-11 pr-12"
					value={searchInput}
					oninput={handleSearchInput}
					aria-label="Search albums"
				/>
				{#if searchInput}
					<button
						class="btn btn-sm btn-ghost btn-circle absolute right-2 top-1/2 -translate-y-1/2"
						onclick={clearSearch}
						aria-label="Clear search"
					>
						<X class="h-4 w-4" />
					</button>
				{/if}
			</div>

			{#if albumsQuery.isError}
				<div class="alert alert-error mb-6">
					<span>Couldn't load albums</span>
					<button class="btn btn-sm btn-ghost" onclick={() => albumsQuery.refetch()}>Retry</button>
				</div>
			{:else if albumsQuery.isLoading}
				<div
					class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-4 xl:grid-cols-5"
				>
					{#each Array(10) as _, i (`skeleton-${i}`)}
						<div class="skeleton aspect-square w-full rounded-lg"></div>
					{/each}
				</div>
			{:else if !albumsQuery.data || albumsQuery.data.items.length === 0}
				<div class="flex min-h-100 flex-col items-center justify-center text-center">
					<Disc3 class="mb-4 h-12 w-12 text-base-content/40" strokeWidth={1.5} />
					<h2 class="mb-2 text-2xl font-semibold">No records found</h2>
					<p class="mb-4 text-base-content/70">
						{params.q || params.format
							? 'Try a different search or filter.'
							: "Your collection doesn't contain any albums yet."}
					</p>
				</div>
			{:else if viewMode === 'wall'}
				<div
					class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-4 xl:grid-cols-5"
				>
					{#each albumsQuery.data.items as album (album.release_group_mbid)}
						<LibraryAlbumCard {album} />
					{/each}
				</div>
			{:else}
				<ul class="flex flex-col gap-1.5" aria-label="Albums">
					{#each albumsQuery.data.items as album (album.release_group_mbid)}
						<li>
							<a
								href={albumHref(album.release_group_mbid)}
								class="group flex items-center gap-3.5 rounded-2xl border border-transparent p-2 transition-colors hover:border-base-content/10 hover:bg-base-200/60"
							>
								<AlbumImage
									mbid={album.release_group_mbid}
									alt={album.album_title}
									size="sm"
									rounded="lg"
									className="h-14 w-14 shrink-0"
								/>
								<div class="min-w-0 flex-1">
									<p class="truncate font-medium group-hover:text-primary">
										{album.album_title}
									</p>
									<p class="truncate text-sm text-base-content/55">
										{album.album_artist_name ?? 'Various artists'}
										{#if album.year}· {album.year}{/if}
									</p>
								</div>
								<div class="hidden shrink-0 items-center gap-3 sm:flex">
									<span class="text-xs tabular-nums text-base-content/40"
										>{album.track_count} tracks · {formatBytes(album.total_size_bytes)}</span
									>
									{#if album.quality_format}
										<span
											class="badge badge-outline badge-sm font-mono text-[0.62rem] uppercase tracking-wider"
											>{album.quality_format}</span
										>
									{/if}
								</div>
							</a>
						</li>
					{/each}
				</ul>
			{/if}

			{#if totalPages > 1 && !albumsQuery.isLoading && !albumsQuery.isError}
				<div class="mt-8 flex justify-center">
					<Pagination
						current={params.page}
						total={totalPages}
						onchange={(p) => setParams({ page: p })}
					/>
				</div>
			{/if}
		</main>
	</div>
</div>

<style>
	:global(.dn-filter-label) {
		margin-bottom: 0.6rem;
		font-family: var(--font-mono);
		font-size: 0.6rem;
		font-weight: 700;
		letter-spacing: 0.2em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.4);
	}
</style>
