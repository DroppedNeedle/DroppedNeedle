<script lang="ts">
	import { onDestroy } from 'svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import SearchAlbumCard from '$lib/features/search/components/SearchAlbumCard.svelte';
	import SearchArtistCard from '$lib/features/search/components/SearchArtistCard.svelte';
	import SearchEmpty from '$lib/features/search/components/SearchEmpty.svelte';
	import SearchHeader from '$lib/features/search/components/SearchHeader.svelte';
	import SearchNotice from '$lib/features/search/components/SearchNotice.svelte';
	import SearchShelf from '$lib/features/search/components/SearchShelf.svelte';
	import SearchSongList from '$lib/features/search/components/SearchSongList.svelte';
	import SearchTopResults from '$lib/features/search/components/SearchTopResults.svelte';
	import ViewMoreTile from '$lib/features/search/components/ViewMoreTile.svelte';
	import MediaTileSkeleton from '$lib/media/MediaTileSkeleton.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import type { EnrichmentSource } from '$lib/features/album/queries/types';
	import type { SearchRemoteStatus } from '$lib/features/search/queries/types';
	import {
		applyArtistEnrichment,
		applyAlbumEnrichment
	} from '$lib/features/search/queries/enrichment';
	import { getSearchStatusNotice } from '$lib/features/search/components/searchStatus';
	import {
		COMBINED_SEARCH_LIMITS,
		getSearchEnrichBatchV3Query,
		getUnifiedSearchV3Query,
		type EnrichmentResponseV3
	} from '$lib/features/search/queries/SearchV3Queries.svelte';
	import { SearchEnrichCollector } from '$lib/features/search/queries/SearchV3Enrichment.svelte';
	import {
		toSearchSongs,
		toV1Album,
		toV1Artist
	} from '$lib/features/search/queries/SearchV3Adapters';
	import { Disc3, Mic } from 'lucide-svelte';

	interface Props {
		data: { query: string };
	}

	let { data }: Props = $props();

	let enrichment: EnrichmentResponseV3 | null = $state(null);
	let enrichmentSource: EnrichmentSource = $state(null);
	let enrichmentQuery = $state('');

	let normalizedQuery = $derived(data.query.trim());
	const searchQuery = getUnifiedSearchV3Query(
		() => normalizedQuery,
		() => COMBINED_SEARCH_LIMITS
	);
	const enrichCollector = new SearchEnrichCollector();
	const enrichQuery = getSearchEnrichBatchV3Query(() => enrichCollector.body);

	let remoteArtists = $derived(
		(() => {
			const results = searchQuery.data?.artists ?? [];
			const top = searchQuery.data?.top_artist;
			if (top && !results.some((artist) => artist.id === top.id)) {
				return [top, ...results.slice(0, COMBINED_SEARCH_LIMITS.artists - 1)];
			}
			return results;
		})()
	);
	let baseArtists = $derived(remoteArtists.map(toV1Artist));
	let baseAlbums = $derived((searchQuery.data?.albums ?? []).map(toV1Album));
	let songs = $derived(toSearchSongs(searchQuery.data?.tracks ?? []));
	let artists = $derived(enrichment ? applyArtistEnrichment(baseArtists, enrichment) : baseArtists);
	let albums = $derived(enrichment ? applyAlbumEnrichment(baseAlbums, enrichment) : baseAlbums);
	let topArtist = $derived.by(() => {
		const top = searchQuery.data?.top_artist;
		if (!top) return null;
		const id = top.musicbrainz_id ?? top.id;
		return artists.find((artist) => artist.musicbrainz_id === id) ?? null;
	});
	let topAlbum = $derived.by(() => {
		const top = searchQuery.data?.top_album;
		if (!top) return null;
		const id = top.musicbrainz_id ?? top.id;
		return albums.find((album) => album.musicbrainz_id === id) ?? null;
	});
	let artistStatus: SearchRemoteStatus = $derived(
		searchQuery.isError ? 'error' : (searchQuery.data?.artist_status ?? 'ok')
	);
	let albumStatus: SearchRemoteStatus = $derived(
		searchQuery.isError ? 'error' : (searchQuery.data?.album_status ?? 'ok')
	);
	let trackStatus: SearchRemoteStatus = $derived(
		searchQuery.isError ? 'error' : (searchQuery.data?.track_status ?? 'ok')
	);
	let artistNotice = $derived(getSearchStatusNotice(artistStatus, 'artists'));
	let albumNotice = $derived(getSearchStatusNotice(albumStatus, 'albums'));
	let trackNotice = $derived(getSearchStatusNotice(trackStatus, 'tracks'));
	let loadingArtists = $derived(searchQuery.isPending && artists.length === 0);
	let loadingAlbums = $derived(searchQuery.isPending && albums.length === 0);
	let hasSearched = $derived(normalizedQuery.length >= 2);

	let isSearching = $derived(searchQuery.isFetching);
	let hasTopResult = $derived(topArtist != null || topAlbum != null);
	let displayedArtists = $derived(
		topArtist ? artists.filter((a) => a.musicbrainz_id !== topArtist?.musicbrainz_id) : artists
	);
	let artistCards = $derived(displayedArtists.slice(0, 6));
	let artistPlaceholderCount = $derived(
		searchQuery.isFetching ? Math.max(0, 6 - artistCards.length) : 0
	);
	let displayedAlbums = $derived(
		topAlbum ? albums.filter((a) => a.musicbrainz_id !== topAlbum?.musicbrainz_id) : albums
	);

	function bucketHref(bucket: 'artists' | 'albums' | 'tracks'): string {
		return withBasePath(`/search/${bucket}?q=${encodeURIComponent(normalizedQuery)}`);
	}

	$effect(() => {
		const result = enrichQuery.data;
		if (result) {
			enrichment = result;
			enrichmentSource = result.source ?? null;
		}
	});

	$effect(() => {
		if (normalizedQuery === enrichmentQuery) return;
		enrichmentQuery = normalizedQuery;
		enrichCollector.reset();
		enrichment = null;
		enrichmentSource = null;
	});

	$effect(() => {
		const handleRefresh = () => {
			void searchQuery.refetch();
		};
		window.addEventListener('search-refresh', handleRefresh);
		return () => window.removeEventListener('search-refresh', handleRefresh);
	});

	onDestroy(() => {
		enrichCollector.dispose();
	});
</script>

<SearchHeader query={hasSearched ? normalizedQuery : ''} current="all" />

<div class="px-4 pb-10 sm:px-6 lg:px-8">
	{#if hasSearched}
		<div class="space-y-10 pt-4">
			{#if artistNotice || albumNotice || trackNotice}
				<div class="space-y-2">
					{#if artistNotice}
						<SearchNotice notice={artistNotice} onretry={() => searchQuery.refetch()} />
					{/if}
					{#if albumNotice}
						<SearchNotice notice={albumNotice} onretry={() => searchQuery.refetch()} />
					{/if}
					{#if trackNotice}
						<SearchNotice notice={trackNotice} onretry={() => searchQuery.refetch()} />
					{/if}
				</div>
			{/if}

			{#if isSearching}
				<div class="grid gap-5 sm:grid-cols-2" aria-label="Loading top search results">
					<div class="skeleton skeleton-shimmer min-h-48 rounded-box"></div>
					<div class="skeleton skeleton-shimmer hidden min-h-48 rounded-box sm:block"></div>
				</div>
			{:else if hasTopResult}
				<section>
					<SectionHead title="Top results" />
					<SearchTopResults
						artist={topArtist}
						album={topAlbum}
						onartistintent={() => topArtist && enrichCollector.requestArtist(topArtist)}
						onalbumintent={() => topAlbum && enrichCollector.requestAlbum(topAlbum)}
					/>
				</section>
			{/if}

			{#if songs.length > 0}
				<section>
					<SectionHead title="Songs" href={bucketHref('tracks')} moreLabel="View more" />
					<SearchSongList {songs} />
				</section>
			{/if}

			<section>
				<SectionHead
					title="Artists"
					href={displayedArtists.length > 0 ? bucketHref('artists') : null}
					moreLabel="View more"
				/>
				{#if loadingArtists}
					<SearchShelf label="Loading artists" swipe busy>
						{#each Array(7) as _, i (`artist-skeleton-${i}`)}
							<MediaTileSkeleton kind="artist" />
						{/each}
					</SearchShelf>
				{:else if displayedArtists.length > 0}
					<SearchShelf label="Artist search results" swipe busy={searchQuery.isFetching}>
						{#each artistCards as artist (artist.musicbrainz_id)}
							<SearchArtistCard
								{artist}
								{enrichmentSource}
								onenrichmentrequest={() => enrichCollector.requestArtist(artist)}
							/>
						{/each}
						{#each Array(artistPlaceholderCount) as _, i (`artist-pending-${i}`)}
							<MediaTileSkeleton kind="artist" />
						{/each}
						<ViewMoreTile kind="artist" href={bucketHref('artists')} what="View more artists" />
					</SearchShelf>
				{:else}
					<SearchEmpty icon={Mic} title="No artists found" compact />
				{/if}
			</section>

			<section>
				<SectionHead
					title="Albums"
					href={displayedAlbums.length > 0 ? bucketHref('albums') : null}
					moreLabel="View more"
				/>
				{#if loadingAlbums}
					<SearchShelf label="Loading albums" swipe busy>
						{#each Array(7) as _, i (`album-skeleton-${i}`)}
							<MediaTileSkeleton />
						{/each}
					</SearchShelf>
				{:else if displayedAlbums.length > 0}
					<SearchShelf label="Album search results" swipe>
						{#each displayedAlbums as album, index (album.musicbrainz_id)}
							<SearchAlbumCard
								{album}
								lazy={index >= 14}
								{enrichmentSource}
								onenrichmentrequest={() => enrichCollector.requestAlbum(album)}
							/>
						{/each}
						<ViewMoreTile kind="album" href={bucketHref('albums')} what="View more albums" />
					</SearchShelf>
				{:else}
					<SearchEmpty icon={Disc3} title="No albums found" compact />
				{/if}
			</section>
		</div>
	{:else}
		<p class="mt-12 text-center text-lg text-muted">Enter a search query to get started.</p>
	{/if}
</div>
