<script lang="ts">
	import './hub.css';
	import { Heart } from 'lucide-svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import HorizontalCarousel from '$lib/ui/HorizontalCarousel.svelte';
	import HubShelf from '$lib/features/remote-library/components/HubShelf.svelte';
	import type { RemoteAlbum, RemoteFavorites } from '$lib/features/remote-library/queries/types';
	import SourceAlbumCardCompact from './SourceAlbumCardCompact.svelte';
	import DiscoveryTrackTable from './DiscoveryTrackTable.svelte';
	import { toDiscoveryTrack } from './remoteTracks';

	interface Props {
		favorites: RemoteFavorites | undefined;
		loading: boolean;
		onopen: (album: RemoteAlbum) => void;
	}

	let { favorites, loading, onopen }: Props = $props();
	let tab = $state<'albums' | 'artists' | 'tracks'>('albums');

	const albums = $derived(favorites?.albums ?? []);
	const artists = $derived(favorites?.artists ?? []);
	const tracks = $derived(favorites?.tracks ?? []);
	const tabs = $derived([
		{ id: 'albums', label: 'Albums', count: albums.length },
		{ id: 'artists', label: 'Artists', count: artists.length },
		{ id: 'tracks', label: 'Tracks', count: tracks.length }
	] as const);
</script>

{#if albums.length > 0 || artists.length > 0 || tracks.length > 0}
	<HubShelf title="Favorites" icon={Heart} {loading}>
		<div role="tablist" aria-label="Favorites" class="hub-pills hub-pills--scroll mb-4">
			{#each tabs as t (t.id)}
				<button
					type="button"
					role="tab"
					class="hub-pill"
					aria-selected={tab === t.id}
					onclick={() => (tab = t.id)}
				>
					{t.label} ({t.count})
				</button>
			{/each}
		</div>

		{#if tab === 'albums'}
			{#if albums.length > 0}
				<!-- two rows that scroll sideways, so the shelf stands as tall as its partner -->
				<HorizontalCarousel class={albums.length > 6 ? 'fav-rows' : ''}>
					{#each albums as album (album.id)}
						<SourceAlbumCardCompact
							imageId={album.release_group_mbid ?? album.id}
							imageUrl={album.image_url}
							name={album.title}
							artistName={album.artist_name}
							onclick={() => onopen(album)}
						/>
					{/each}
				</HorizontalCarousel>
			{:else}
				<p class="text-sm text-muted">No favorite albums yet.</p>
			{/if}
		{:else if tab === 'artists'}
			{#if artists.length > 0}
				<HorizontalCarousel class={artists.length > 6 ? 'fav-rows' : ''}>
					{#each artists as artist (artist.id)}
						<MediaTile kind="artist" title={artist.name} class="w-32 shrink-0">
							{#snippet art()}
								<ArtistImage
									mbid={artist.artist_mbid ?? artist.id}
									remoteUrl={artist.image_url}
									alt={artist.name}
									size="full"
									requestSize={250}
									rounded="full"
									className="h-full w-full"
								/>
							{/snippet}
							{#snippet meta()}
								{artist.album_count ?? 0} album{(artist.album_count ?? 0) !== 1 ? 's' : ''}
							{/snippet}
						</MediaTile>
					{/each}
				</HorizontalCarousel>
			{:else}
				<p class="text-sm text-muted">No favorite artists yet.</p>
			{/if}
		{:else if tracks.length > 0}
			<DiscoveryTrackTable tracks={tracks.map(toDiscoveryTrack)} />
		{:else}
			<p class="text-sm text-muted">No favorite tracks yet.</p>
		{/if}
	</HubShelf>
{/if}

<style>
	@media (min-width: 640px) {
		:global(.fav-rows) {
			display: grid !important;
			grid-auto-columns: 9rem;
			grid-auto-flow: column;
			grid-template-rows: repeat(2, auto);
			row-gap: 1.25rem;
		}
	}
</style>
