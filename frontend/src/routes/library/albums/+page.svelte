<script lang="ts">
	import { untrack } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { Disc3, Heart, Search, X } from 'lucide-svelte';
	import {
		getLibraryAlbumBlocksQuery,
		getLibraryAlbumFormatsQuery,
		getLibraryAlbumLettersQuery
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import LibraryFilterMenu, {
		type FormatMode,
		type RatingMode,
		type TracksMode
	} from '$lib/features/library/components/LibraryFilterMenu.svelte';
	import {
		keepFilter,
		keptFilter,
		rememberFilters
	} from '$lib/features/library/state/rememberedFilters.svelte';
	import type { LibraryV3AlbumsParams } from '$lib/features/library/queries/LibraryQueryKeyFactory';
	import MediaTileSkeleton from '$lib/media/MediaTileSkeleton.svelte';
	import LibraryAlbumCard from '$lib/features/library/components/LibraryAlbumCard.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import Input from '$lib/ui/Input.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import ScrollRail from '$lib/ui/ScrollRail.svelte';
	import VirtualRows, { type VirtualRange } from '$lib/ui/VirtualRows.svelte';
	import type { AlbumView } from '$lib/features/library/queries/types';
	import type { Snapshot } from './$types';

	// Albums are read in blocks by their position in the list.
	const BLOCK = 60;
	const LINE_PITCH = 290;
	const SEARCH_DEBOUNCE_MS = 300;
	// `hearted` is the favourites' own order: the newest heart first.
	type Sort = 'recent' | 'title' | 'artist' | 'hearted';
	const VALID_SORTS: Sort[] = ['recent', 'title', 'artist'];

	function isSort(value: string | null): value is Sort {
		return VALID_SORTS.some((sort) => sort === value);
	}

	// All Albums starts on the sort this browser last chose here, in place before the
	// first render; the favourites view has its own order and never writes this.
	const keptSort = keptFilter('album-sort', 'sort');
	let defaultSort = $state<Sort>(isSort(keptSort) ? keptSort : 'recent');

	// browse state lives in the url so it's shareable and back-button stable
	const params = $derived.by(() => {
		const sp = page.url.searchParams;
		const favorites = sp.get('favorites') === '1';
		const rawSort = sp.get('sort') as Sort | null;
		const valid =
			rawSort && (VALID_SORTS.includes(rawSort) || (favorites && rawSort === 'hearted'));
		return {
			sort: valid ? rawSort : favorites ? 'hearted' : defaultSort,
			q: sp.get('q') ?? '',
			// `format` is the one-format link of before the filter menu.
			formats: (sp.get('formats') ?? sp.get('format') ?? '')
				.split(',')
				.map((format) => format.trim().toLowerCase())
				.filter(Boolean),
			formatMode: (sp.get('format_mode') === 'without' ? 'without' : 'with') as FormatMode,
			minTracks: Math.max(1, Number(sp.get('min_tracks')) || 1),
			tracksMode: (sp.get('tracks_mode') === 'below' ? 'below' : 'atLeast') as TracksMode,
			rating: Math.min(5, Math.max(0, Number(sp.get('rating')) || 0)),
			ratingMode: (sp.get('rating_mode') === 'at_most' ? 'atMost' : 'atLeast') as RatingMode,
			favorites
		};
	});

	// The url keeps the v1 sort vocabulary so shared links survive; it maps
	// onto the v3 catalog sorts one row below.
	const SORT_MAP: Record<Sort, Pick<LibraryV3AlbumsParams, 'sort' | 'order'>> = {
		recent: { sort: 'date_added', order: 'desc' },
		title: { sort: 'name', order: 'asc' },
		artist: { sort: 'artist', order: 'asc' },
		hearted: {}
	};

	const albumParams = $derived<LibraryV3AlbumsParams>({
		limit: BLOCK,
		...SORT_MAP[params.sort],
		...(params.q ? { q: params.q } : {}),
		...(params.formats.length ? { formats: params.formats, formatMode: params.formatMode } : {}),
		...(params.minTracks > 1
			? { minTracks: params.minTracks, tracksBelow: params.tracksMode === 'below' }
			: {}),
		...(params.rating > 0
			? {
					rating: params.rating,
					ratingMode: params.ratingMode === 'atMost' ? ('at_most' as const) : ('at_least' as const)
				}
			: {}),
		...(params.favorites ? { favorites: true } : {})
	});
	const named = $derived(params.sort === 'title' || params.sort === 'artist');

	let range = $state<VirtualRange>({ start: 0, end: 0, top: 0 });
	// The blocks the mounted tiles fall in, and always the first, which
	// carries the total.
	const offsets = $derived.by(() => {
		const out = [0];
		const last = Math.max(0, range.end - 1);
		for (let at = Math.floor(range.start / BLOCK) * BLOCK; at <= last; at += BLOCK) {
			if (at > 0) out.push(at);
		}
		return out;
	});
	const blocks = getLibraryAlbumBlocksQuery(
		() => albumParams,
		() => offsets
	);
	const firstBlock = $derived(blocks[0]);
	const lettersQuery = getLibraryAlbumLettersQuery(
		() => albumParams,
		() => named
	);
	const formatsQuery = getLibraryAlbumFormatsQuery();
	rememberFilters('albums', [
		'formats',
		'format_mode',
		'min_tracks',
		'tracks_mode',
		'rating',
		'rating_mode'
	]);
	const filtered = $derived(params.formats.length > 0 || params.minTracks > 1 || params.rating > 0);
	const total = $derived(firstBlock?.data?.total ?? 0);

	function albumAt(index: number): AlbumView | undefined {
		const at = Math.floor(index / BLOCK) * BLOCK;
		return blocks[offsets.indexOf(at)]?.data?.items[index - at];
	}

	let list = $state<VirtualRows>();

	// A new search, order or filter starts the list at the top.
	let shownList = '';
	$effect(() => {
		const key = JSON.stringify(albumParams);
		if (shownList && key !== shownList) {
			untrack(() => {
				if (range.top > 0) list?.scrollToIndex(0);
			});
		}
		shownList = key;
	});

	// Back to the list lands on the album that was at the top.
	let restoreTop = $state<number | null>(null);
	export const snapshot: Snapshot<number> = {
		capture: () => range.top,
		restore: (top) => (restoreTop = top)
	};
	$effect(() => {
		if (restoreTop === null || total === 0 || !list) return;
		if (Math.abs(range.top - restoreTop) > 1) list.scrollToIndex(restoreTop);
		restoreTop = null;
	});

	// local edits win over the url term until back/forward navigation re-syncs the input
	let searchInput = $derived(params.q);
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;
	$effect(() => () => clearTimeout(searchTimeout));

	function setParams(updates: Record<string, string | null>) {
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
		searchTimeout = setTimeout(() => setParams({ q: searchInput.trim() }), SEARCH_DEBOUNCE_MS);
	}

	function clearSearch() {
		searchInput = '';
		clearTimeout(searchTimeout);
		setParams({ q: null });
	}

	function toggleFavorites() {
		setParams({ favorites: params.favorites ? null : '1', sort: null });
	}

	function setSort(value: string) {
		setParams({ sort: value });
		// Only the All Albums order is the default; the favourites view has its own.
		if (!params.favorites && isSort(value)) {
			defaultSort = value;
			keepFilter('album-sort', 'sort', value);
		}
	}
</script>

<svelte:head><title>Albums - DroppedNeedle</title></svelte:head>

<ListPageHeader
	title={params.favorites ? 'Favourite Albums' : 'All Albums'}
	count={firstBlock?.data ? `${total.toLocaleString()} ${total === 1 ? 'album' : 'albums'}` : null}
	ticket="DN / CC 3.2 - ALB"
	backHref={withBasePath('/library')}
	backLabel="Back to library"
/>

<div class="px-4 pb-10 sm:px-6 lg:px-8">
	<div class="dn-toolbar">
		<div class="dn-toolbar__grow">
			<Input
				icon={Search}
				label="Search albums"
				placeholder="Search albums or artists..."
				class="w-full pr-12"
				value={searchInput}
				oninput={handleSearchInput}
			/>
			{#if searchInput}
				<IconButton
					size="sm"
					icon={X}
					label="Clear search"
					class="absolute right-2 top-1/2 -translate-y-1/2"
					onclick={clearSearch}
				/>
			{/if}
		</div>
		<Button
			variant={params.favorites ? 'primary' : 'outline'}
			icon={Heart}
			aria-pressed={params.favorites}
			onclick={toggleFavorites}
		>
			Favourites
		</Button>
		<select
			class="select dn-toolbar__select"
			value={params.sort}
			onchange={(e) => setSort((e.target as HTMLSelectElement).value)}
			aria-label="Sort albums"
		>
			{#if params.favorites}<option value="hearted">Recently Favourited</option>{/if}
			<option value="recent">Newest First</option>
			<option value="title">Title A-Z</option>
			<option value="artist">Artist A-Z</option>
		</select>
		<LibraryFilterMenu
			minTracks={params.minTracks}
			tracksMode={params.tracksMode}
			onMinTracks={(min, mode) =>
				setParams({
					min_tracks: min > 1 ? String(min) : null,
					tracks_mode: mode === 'below' && min > 1 ? 'below' : null
				})}
			rating={params.rating}
			ratingMode={params.ratingMode}
			onRating={(stars, mode) =>
				setParams({
					rating: stars > 0 ? String(stars) : null,
					rating_mode: mode === 'atMost' && stars > 0 ? 'at_most' : null
				})}
			formatCounts={formatsQuery.data ?? []}
			formats={params.formats}
			formatMode={params.formatMode}
			onFormats={(formats, mode) =>
				setParams({
					format: null,
					formats: formats.length ? formats.join(',') : null,
					format_mode: mode === 'without' ? 'without' : null
				})}
		/>
	</div>

	<QueryBoundary query={firstBlock} what="your albums" isEmpty={(data) => data.items.length === 0}>
		{#snippet loading()}
			<div class="dn-list-grid">
				{#each Array(14) as _, i (`skeleton-${i}`)}
					<div class="skeleton aspect-square w-full rounded-lg"></div>
				{/each}
			</div>
		{/snippet}
		{#snippet empty()}
			<div class="flex flex-col items-center justify-center min-h-100 text-center">
				<Disc3 class="h-12 w-12 text-muted mb-4" strokeWidth={1.5} />
				<h2 class="text-2xl font-semibold mb-2">
					{params.favorites && !params.q && !filtered ? 'No favourites yet' : 'No albums found'}
				</h2>
				<p class="text-muted mb-4">
					{params.q || filtered
						? 'Try a different search or filter.'
						: params.favorites
							? 'Tap the heart on an album to add it here.'
							: "Your library doesn't contain any albums yet."}
				</p>
			</div>
		{/snippet}
		<div class="flex items-start gap-2">
			<div class="min-w-0 flex-1">
				<VirtualRows
					bind:this={list}
					count={total}
					pitch={LINE_PITCH}
					layout="grid"
					label={params.favorites ? 'Favourite albums' : 'All albums'}
					class="dn-list-grid"
					onrange={(next) => (range = next)}
				>
					{#snippet item(index)}
						{@const album = albumAt(index)}
						{#if album}
							<LibraryAlbumCard {album} />
						{:else}
							<div data-placeholder tabindex="-1" aria-busy="true" aria-label="Loading">
								<MediaTileSkeleton />
							</div>
						{/if}
					{/snippet}
				</VirtualRows>
			</div>
			<ScrollRail
				letters={named ? lettersQuery.data?.letters : null}
				{total}
				top={range.top}
				onjump={(position) => list?.scrollToIndex(position)}
			/>
		</div>
	</QueryBoundary>
</div>
