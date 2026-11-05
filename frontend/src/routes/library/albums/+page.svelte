<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { Disc3, Heart, Search, X } from 'lucide-svelte';
	import {
		getLibraryAlbumFormatsQuery,
		getLibraryAlbumsInfiniteQuery
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import LibraryFilterMenu, {
		type FormatMode,
		type RatingMode,
		type TracksMode
	} from '$lib/features/library/components/LibraryFilterMenu.svelte';
	import { rememberFilters } from '$lib/features/library/state/rememberedFilters.svelte';
	import type { LibraryV3AlbumsParams } from '$lib/features/library/queries/LibraryQueryKeyFactory';
	import LibraryAlbumCard from '$lib/features/library/components/LibraryAlbumCard.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import Input from '$lib/ui/Input.svelte';
	import type { AlbumView } from '$lib/features/library/queries/types';

	const PAGE_SIZE = 50;
	const SEARCH_DEBOUNCE_MS = 300;
	// `hearted` is the favourites' own order: the newest heart first.
	type Sort = 'recent' | 'title' | 'artist' | 'hearted';
	const VALID_SORTS: Sort[] = ['recent', 'title', 'artist'];

	// browse state lives in the url so it's shareable and back-button stable
	const params = $derived.by(() => {
		const sp = page.url.searchParams;
		const favorites = sp.get('favorites') === '1';
		const rawSort = sp.get('sort') as Sort | null;
		const valid =
			rawSort && (VALID_SORTS.includes(rawSort) || (favorites && rawSort === 'hearted'));
		return {
			sort: valid ? rawSort : favorites ? 'hearted' : 'recent',
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

	const albumsQuery = getLibraryAlbumsInfiniteQuery(() => ({
		limit: PAGE_SIZE,
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
	}));
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
	// An album can move between pages while they load (a scan renames it):
	// keep its first appearance.
	const albums = $derived.by(() => {
		const seen: Record<string, true> = Object.create(null);
		const out: AlbumView[] = [];
		for (const response of albumsQuery.data?.pages ?? []) {
			for (const album of response.items) {
				if (seen[album.id]) continue;
				seen[album.id] = true;
				out.push(album);
			}
		}
		return out;
	});
	const total = $derived(albumsQuery.data?.pages[0]?.total ?? 0);

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
</script>

<svelte:head><title>Albums · Library</title></svelte:head>

<ListPageHeader
	title={params.favorites ? 'Favourite Albums' : 'All Albums'}
	count={albumsQuery.data ? `${total.toLocaleString()} ${total === 1 ? 'album' : 'albums'}` : null}
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
			onclick={toggleFavorites}>Favourites</Button
		>
		<select
			class="select dn-toolbar__select"
			value={params.sort}
			onchange={(e) => setParams({ sort: (e.target as HTMLSelectElement).value })}
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

	{#if albumsQuery.isError}
		<div class="alert alert-error mb-6">
			<span>Couldn't load albums</span>
			<Button variant="ghost" size="sm" onclick={() => albumsQuery.refetch()}>Retry</Button>
		</div>
	{:else if albumsQuery.isLoading}
		<div class="dn-list-grid">
			{#each Array(14) as _, i (`skeleton-${i}`)}
				<div class="skeleton aspect-square w-full rounded-lg"></div>
			{/each}
		</div>
	{:else if albums.length === 0}
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
	{:else}
		<div class="dn-list-grid">
			{#each albums as album (album.id)}
				<LibraryAlbumCard {album} />
			{/each}
		</div>

		{#if albumsQuery.hasNextPage}
			<div class="mt-8 flex justify-center">
				<Button
					variant="outline"
					class="min-w-60"
					loading={albumsQuery.isFetchingNextPage}
					onclick={() => albumsQuery.fetchNextPage()}
				>
					Load more ({albums.length} / {total})
				</Button>
			</div>
		{/if}
	{/if}
</div>
