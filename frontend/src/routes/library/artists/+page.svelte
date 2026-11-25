<script lang="ts">
	import { untrack } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import ArtistCardSkeleton from '$lib/features/artist/components/ArtistCardSkeleton.svelte';
	import MediaTileSkeleton from '$lib/media/MediaTileSkeleton.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import LocalIdentityBadge from '$lib/features/library/components/LocalIdentityBadge.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import ScrollRail from '$lib/ui/ScrollRail.svelte';
	import VirtualRows, { type VirtualRange } from '$lib/ui/VirtualRows.svelte';
	import Tabs, { type TabItem } from '$lib/ui/Tabs.svelte';
	import {
		getLibraryArtistBlocksQuery,
		getLibraryArtistLettersQuery,
		libraryArtistParams
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import type {
		ArtistSort,
		LibraryArtistScope,
		ArtistView
	} from '$lib/features/library/queries/types';
	import { artistHref } from '$lib/utils/entityRoutes';
	import { Disc3, Heart, Mic, Search, UsersRound, X } from 'lucide-svelte';
	import LibraryFilterMenu, {
		type RatingMode,
		type TracksMode
	} from '$lib/features/library/components/LibraryFilterMenu.svelte';
	import { rememberFilters } from '$lib/features/library/state/rememberedFilters.svelte';
	import type { Snapshot } from './$types';

	const SEARCH_DEBOUNCE_MS = 300;
	// Artists are read in blocks by their position in the list.
	const BLOCK = 60;
	const LINE_PITCH = 230;

	// `hearted` is the favourites' own order: the newest heart first.
	type Sort = ArtistSort | 'hearted';

	const browse = $derived.by(() => {
		const view = page.url.searchParams.get('view') === 'contributors' ? 'contributors' : 'albums';
		const scope: LibraryArtistScope = view === 'contributors' ? 'contributors' : 'album_artists';
		const favorites = page.url.searchParams.get('favorites') === '1';
		const rawSort = page.url.searchParams.get('sort') as Sort | null;
		const validSorts: Sort[] = [
			...(favorites ? (['hearted'] as const) : []),
			...(scope === 'contributors'
				? (['name', 'appearance_count', 'date_added'] as const)
				: (['name', 'album_count', 'date_added'] as const))
		];
		const defaultSort: Sort = favorites ? 'hearted' : 'name';
		return {
			view,
			scope,
			favorites,
			defaultSort,
			q: page.url.searchParams.get('q') ?? '',
			minTracks: Math.max(1, Number(page.url.searchParams.get('min_tracks')) || 1),
			tracksMode: (page.url.searchParams.get('tracks_mode') === 'below'
				? 'below'
				: 'atLeast') as TracksMode,
			rating: Math.min(5, Math.max(0, Number(page.url.searchParams.get('rating')) || 0)),
			ratingMode: (page.url.searchParams.get('rating_mode') === 'at_most'
				? 'atMost'
				: 'atLeast') as RatingMode,
			sortBy: rawSort && validSorts.includes(rawSort) ? rawSort : defaultSort,
			sortOrder: page.url.searchParams.get('order') === 'desc' ? 'desc' : 'asc'
		} as const;
	});

	const params = $derived({
		sortBy: browse.sortBy,
		sortOrder: browse.sortOrder,
		q: browse.q,
		scope: browse.scope,
		minTracks: browse.minTracks,
		tracksBelow: browse.tracksMode === 'below',
		rating: browse.rating,
		ratingMode: browse.ratingMode === 'atMost' ? ('at_most' as const) : ('at_least' as const),
		favorites: browse.favorites
	});
	rememberFilters('artists', ['min_tracks', 'tracks_mode', 'rating', 'rating_mode']);

	const artistParams = $derived(libraryArtistParams(params, BLOCK));
	const named = $derived(browse.sortBy === 'name');

	let range = $state<VirtualRange>({ start: 0, end: 0, top: 0 });
	// The blocks the mounted tiles fall in, and always the first, which
	// carries the totals.
	const offsets = $derived.by(() => {
		const out = [0];
		const last = Math.max(0, range.end - 1);
		for (let at = Math.floor(range.start / BLOCK) * BLOCK; at <= last; at += BLOCK) {
			if (at > 0) out.push(at);
		}
		return out;
	});
	const blocks = getLibraryArtistBlocksQuery(
		() => artistParams,
		() => offsets
	);
	const firstBlock = $derived(blocks[0]);
	const lettersQuery = getLibraryArtistLettersQuery(
		() => artistParams,
		() => named
	);

	function artistAt(index: number): ArtistView | undefined {
		const at = Math.floor(index / BLOCK) * BLOCK;
		return blocks[offsets.indexOf(at)]?.data?.items[index - at];
	}

	const response = $derived(firstBlock?.data);
	const total = $derived(response?.total ?? 0);

	let list = $state<VirtualRows>();

	// A new view, search, order or filter starts the list at the top.
	let shownList = '';
	$effect(() => {
		const key = JSON.stringify(artistParams);
		if (shownList && key !== shownList) {
			untrack(() => {
				if (range.top > 0) list?.scrollToIndex(0);
			});
		}
		shownList = key;
	});

	// Back to the list lands on the artist that was at the top.
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
	const albumArtistTotal = $derived(response?.album_artist_total ?? 0);
	const contributorTotal = $derived(response?.contributor_total ?? 0);
	const viewTabs = $derived<TabItem<'albums' | 'contributors'>[]>([
		{ id: 'albums', label: 'Album artists', icon: Disc3, count: albumArtistTotal },
		{ id: 'contributors', label: 'Contributors', icon: UsersRound, count: contributorTotal }
	]);

	let searchInput = $derived(browse.q);
	let searchTimeout: ReturnType<typeof setTimeout> | undefined;
	$effect(() => () => clearTimeout(searchTimeout));

	function setParams(updates: Record<string, string | null>): void {
		const url = new URL(page.url);
		for (const [key, value] of Object.entries(updates)) {
			if (!value) url.searchParams.delete(key);
			else url.searchParams.set(key, value);
		}
		void goto(url, { replaceState: true, keepFocus: true, noScroll: true });
	}

	function selectView(view: 'albums' | 'contributors'): void {
		setParams({ view: view === 'albums' ? null : view, sort: null, order: null });
	}

	function handleSearchInput(event: Event): void {
		searchInput = (event.target as HTMLInputElement).value;
		clearTimeout(searchTimeout);
		searchTimeout = setTimeout(
			() => setParams({ q: searchInput.trim() || null }),
			SEARCH_DEBOUNCE_MS
		);
	}

	function clearSearch(): void {
		searchInput = '';
		clearTimeout(searchTimeout);
		setParams({ q: null });
	}

	function handleSortChange(event: Event): void {
		const [sort, order] = (event.target as HTMLSelectElement).value.split(':') as [
			Sort,
			'asc' | 'desc'
		];
		setParams({
			sort: sort === browse.defaultSort ? null : sort,
			order: order === 'asc' ? null : order
		});
	}

	function toggleFavorites(): void {
		setParams({ favorites: browse.favorites ? null : '1', sort: null, order: null });
	}
</script>

<svelte:head><title>Artists - DroppedNeedle</title></svelte:head>

<ListPageHeader
	title={browse.favorites ? 'Favourite Artists' : 'All Artists'}
	count={response ? `${total.toLocaleString()} ${total === 1 ? 'artist' : 'artists'}` : null}
	ticket="DN / CC 3.2 - ART"
	backHref={withBasePath('/library')}
	backLabel="Back to library"
/>

<main class="px-4 pb-10 sm:px-6 lg:px-8">
	<Tabs
		tabs={viewTabs}
		selected={browse.view}
		onselect={selectView}
		label="Artist library views"
		idPrefix="artist-views"
		class="dn-toolbar"
	>
		{#snippet aside()}
			<div class="dn-toolbar__grow">
				<Input
					icon={Search}
					label="Search artists"
					placeholder={browse.scope === 'album_artists'
						? 'Search album artists…'
						: 'Search contributors…'}
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
				variant={browse.favorites ? 'primary' : 'outline'}
				icon={Heart}
				aria-pressed={browse.favorites}
				onclick={toggleFavorites}>Favourites</Button
			>
			<select
				class="select dn-toolbar__select"
				value="{browse.sortBy}:{browse.sortOrder}"
				onchange={handleSortChange}
				aria-label="Sort artists"
			>
				{#if browse.favorites}<option value="hearted:asc">Recently favourited</option>{/if}
				<option value="name:asc">Name A-Z</option>
				<option value="name:desc">Name Z-A</option>
				{#if browse.scope === 'album_artists'}
					<option value="album_count:desc">Most releases</option>
					<option value="album_count:asc">Fewest releases</option>
				{:else}
					<option value="appearance_count:desc">Most appearances</option>
					<option value="appearance_count:asc">Fewest appearances</option>
				{/if}
				<option value="date_added:desc">Newest first</option>
				<option value="date_added:asc">Oldest first</option>
			</select>
			<LibraryFilterMenu
				minTracks={browse.minTracks}
				tracksMode={browse.tracksMode}
				tracksLabel={browse.scope === 'album_artists' ? 'Tracks on their albums' : 'Appearances'}
				onMinTracks={(min, mode) =>
					setParams({
						min_tracks: min > 1 ? String(min) : null,
						tracks_mode: mode === 'below' && min > 1 ? 'below' : null
					})}
				rating={browse.rating}
				ratingMode={browse.ratingMode}
				onRating={(stars, mode) =>
					setParams({
						rating: stars > 0 ? String(stars) : null,
						rating_mode: mode === 'atMost' && stars > 0 ? 'at_most' : null
					})}
			/>
		{/snippet}

		<p class="mb-4 font-mono text-xs tracking-wider text-muted uppercase">
			{browse.scope === 'album_artists'
				? 'The artists who own releases in your library'
				: 'Guests, featured artists and track-level credits'}
		</p>

		<QueryBoundary
			query={firstBlock}
			what="your artists"
			isEmpty={(data) => data.items.length === 0}
		>
			{#snippet loading()}
				<div class="dn-list-grid">
					{#each Array(14) as _, index (`skeleton-${index}`)}<ArtistCardSkeleton />{/each}
				</div>
			{/snippet}
			{#snippet empty()}
				<div class="flex min-h-100 flex-col items-center justify-center text-center">
					<Mic class="mb-4 h-12 w-12 text-base-content/35" strokeWidth={1.5} />
					<h2 class="mb-2 text-2xl font-semibold">
						{browse.q
							? 'No matching artists'
							: browse.favorites
								? 'No favourites here yet'
								: 'Nothing in this view yet'}
					</h2>
					<p class="max-w-md text-muted">
						{browse.q
							? 'Try a different search term.'
							: browse.favorites
								? 'Tap the heart on an artist to add them here.'
								: browse.scope === 'album_artists'
									? 'Album artists appear here as releases are added to your library.'
									: 'Track-level guests and featured artists will appear here.'}
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
						label={browse.scope === 'album_artists' ? 'Album artists' : 'Contributors'}
						class="dn-list-grid"
						onrange={(next) => (range = next)}
					>
						{#snippet item(index)}
							{@const artist = artistAt(index)}
							{#if artist}
								<MediaTile
									kind="artist"
									title={artist.name}
									titleAs="h2"
									href={artistHref(artist.artist_mbid ?? artist.id)}
									label={`Open ${artist.name}`}
								>
									{#snippet art()}
										<ArtistImage
											mbid={artist.artist_mbid ?? ''}
											localId={artist.id}
											artMissing={artist.art_missing}
											alt={artist.name}
											size="full"
											requestSize={250}
											className="h-full w-full"
										/>
									{/snippet}
									{#snippet meta()}
										{#if artist.disambiguation}{artist.disambiguation} ·{/if}
										{#if browse.scope === 'album_artists'}
											{artist.album_count}
											{artist.album_count === 1 ? 'release' : 'releases'} ·
											{artist.led_track_count}
											{artist.led_track_count === 1 ? 'track' : 'tracks'}
										{:else}
											{artist.appearance_album_count}
											{artist.appearance_album_count === 1 ? 'release' : 'releases'} ·
											{artist.appearance_track_count}
											{artist.appearance_track_count === 1 ? 'appearance' : 'appearances'}
										{/if}
									{/snippet}
									{#snippet overlay()}
										{#if artist.identity_state === 'local_only'}
											<LocalIdentityBadge
												state={artist.identity_state}
												subject="artist"
												compact
												className="absolute left-1 top-1 z-10"
											/>
										{/if}
									{/snippet}
								</MediaTile>
							{:else}
								<div data-placeholder tabindex="-1" aria-busy="true" aria-label="Loading">
									<MediaTileSkeleton kind="artist" />
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
	</Tabs>
</main>
