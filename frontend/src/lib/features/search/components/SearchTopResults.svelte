<!--
	The top artist and top album, side by side; on a phone a sideways swipe
	of the two with page dots under it.
-->
<script lang="ts">
	import { ArrowRight, Download } from 'lucide-svelte';
	import type { Album } from '$lib/features/album/queries/types';
	import type { Artist } from '$lib/features/search/queries/types';
	import AlbumCardOverlay from '$lib/features/album/components/AlbumCardOverlay.svelte';
	import { imageMbid, isLocalOnlyRow } from '$lib/features/search/queries/SearchV3Adapters';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import SearchLibraryChip from './SearchLibraryChip.svelte';
	import SearchTopCard from './SearchTopCard.svelte';
	import {
		albumInLibrary,
		albumRequested,
		createSearchAlbumRequest,
		isLocalOnlyAlbum
	} from './searchAlbum.svelte';
	import { openDiscography } from './searchDiscography';

	interface Props {
		artist?: Artist | null;
		album?: Album | null;
		onartistintent?: () => void;
		onalbumintent?: () => void;
	}

	let { artist = null, album = null, onartistintent, onalbumintent }: Props = $props();

	let loadingDiscography = $state(false);
	let removed = $state(false);
	let shown = $state(0);
	const request = createSearchAlbumRequest();

	const both = $derived(artist != null && album != null);
	const artistLocal = $derived(artist ? isLocalOnlyRow(artist) : false);
	const artistLine = $derived(
		artist ? [artist.type_info, artist.disambiguation].filter(Boolean).join(' · ') : ''
	);
	const albumLocal = $derived(album ? isLocalOnlyAlbum(album) : false);
	const inLibrary = $derived(album ? albumInLibrary(album, removed) : false);
	const requested = $derived(album ? albumRequested(album, removed) : false);
	const localAlbumId = $derived(album && albumLocal ? (album.local_id ?? undefined) : undefined);

	async function discography(name: string, mbid: string) {
		if (loadingDiscography) return;
		loadingDiscography = true;
		try {
			await openDiscography(name, mbid);
		} finally {
			loadingDiscography = false;
		}
	}

	function trackSwipe(event: Event) {
		const row = event.currentTarget as HTMLElement;
		const room = row.scrollWidth - row.clientWidth;
		shown = room > 0 && row.scrollLeft > room / 2 ? 1 : 0;
	}
</script>

<div class="search-tops" class:search-tops--both={both} onscroll={trackSwipe}>
	{#if artist}
		<SearchTopCard
			kicker="Top artist"
			title={artist.title}
			href={artistHref(artist.musicbrainz_id)}
			onintent={artistLocal ? undefined : onartistintent}
		>
			{#snippet art()}
				<ArtistImage
					mbid={imageMbid(artist)}
					alt={artist.title}
					size="full"
					requestSize={250}
					lazy={false}
					remoteUrl={artist.thumb_url ?? null}
					source={artist.local_id ? 'local' : 'provider'}
					available={!artistLocal}
					localId={artistLocal ? artist.local_id : null}
					className="aspect-square w-full"
				/>
			{/snippet}
			{#snippet lines()}
				{#if artistLine}<p class="font-sans">{artistLine}</p>{/if}
				<!-- kept while empty, so the count fills in place -->
				<p class="tracking-[0.14em] uppercase">
					{#if artist.release_group_count != null}
						{artist.release_group_count}
						{artist.release_group_count === 1 ? 'release' : 'releases'}
					{:else}&nbsp;{/if}
				</p>
			{/snippet}
			{#snippet actions()}
				{#if !artist.local_id || artist.local_id !== artist.musicbrainz_id}
					<Button
						variant="paper"
						size="display-sm"
						icon={Download}
						loading={loadingDiscography}
						onclick={() => discography(artist.title, artist.musicbrainz_id)}
						>Download discography</Button
					>
				{/if}
			{/snippet}
		</SearchTopCard>
	{/if}
	{#if album}
		<SearchTopCard
			kicker="Top album"
			title={album.title}
			href={albumHref(album.musicbrainz_id)}
			onintent={albumLocal ? undefined : onalbumintent}
		>
			{#snippet art()}
				<AlbumImage
					albumId={localAlbumId}
					mbid={albumLocal ? '' : album.musicbrainz_id}
					customUrl={album.cover_url}
					remoteUrl={album.album_thumb_url ?? null}
					artMissing={album.art_missing}
					alt={album.title}
					size="full"
					requestSize={250}
					lazy={false}
					rounded="sm"
					className="aspect-square w-full"
				/>
				{#if inLibrary && !albumLocal}
					<AlbumCardOverlay
						mbid={album.musicbrainz_id}
						albumName={album.title}
						artistName={album.artist || 'Unknown'}
						coverUrl={album.cover_url ?? album.album_thumb_url ?? null}
						size="sm"
						playShown
					/>
				{/if}
			{/snippet}
			{#snippet lines()}
				<p>
					{#if album.year}{album.year}{/if}{#if album.year && album.artist}<span
							class="mx-2"
							aria-hidden="true">·</span
						>{/if}{#if album.artist}{album.artist}{/if}
				</p>
				{#if !albumLocal && (inLibrary || requested)}
					<div class="relative z-2">
						<SearchLibraryChip
							status={inLibrary ? 'library' : 'requested'}
							musicbrainzId={album.musicbrainz_id}
							albumTitle={album.title}
							artistName={album.artist || 'Unknown'}
							ondeleted={() => (removed = true)}
						/>
					</div>
				{/if}
			{/snippet}
			{#snippet actions()}
				{#if !inLibrary && !requested}
					<Button
						variant="primary"
						size="display-sm"
						icon={Download}
						loading={request.pending}
						onclick={() => void request.request(album)}>Request</Button
					>
				{/if}
				<Button
					variant="paper"
					size="display-sm"
					icon={ArrowRight}
					iconPosition="end"
					href={albumHref(album.musicbrainz_id)}>View album</Button
				>
			{/snippet}
		</SearchTopCard>
	{/if}
</div>
{#if both}
	<div class="search-tops__dots" aria-hidden="true">
		<span class:is-on={shown === 0}></span>
		<span class:is-on={shown === 1}></span>
	</div>
{/if}

<style>
	.search-tops {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 1.25rem;
	}
	.search-tops:not(.search-tops--both) {
		grid-template-columns: minmax(0, 1fr);
		max-width: 48rem;
	}

	.search-tops__dots {
		display: none;
	}

	@media (max-width: 639px) {
		.search-tops--both {
			grid-template-columns: none;
			grid-auto-flow: column;
			grid-auto-columns: 88%;
			gap: 0.75rem;
			margin-inline: -1rem;
			padding-inline: 1rem;
			overflow-x: auto;
			scroll-padding-inline: 1rem;
			scroll-snap-type: x mandatory;
			scrollbar-width: none;
		}
		.search-tops--both > :global(*) {
			scroll-snap-align: start;
		}
		.search-tops__dots {
			display: flex;
			justify-content: center;
			gap: 0.5rem;
			margin-top: 0.75rem;
		}
		.search-tops__dots span {
			width: 0.5rem;
			height: 0.5rem;
			border-radius: 9999px;
			background-color: color-mix(in oklab, var(--color-muted) 55%, transparent);
		}
		.search-tops__dots span.is-on {
			background-color: var(--dn-title);
		}
	}
</style>
