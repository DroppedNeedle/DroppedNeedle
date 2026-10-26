<script lang="ts">
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import HorizontalCarousel from '$lib/components/HorizontalCarousel.svelte';
	import HubShelf from '$lib/components/HubShelf.svelte';
	import { reveal } from '$lib/actions/reveal';
	import { formatDurationSec } from '$lib/utils/formatting';
	import type { RemoteAlbum, RemoteFavorites } from '$lib/queries/remotes/types';
	import AlbumCarousel from './AlbumCarousel.svelte';

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
</script>

{#if albums.length > 0 || artists.length > 0 || tracks.length > 0}
	<div use:reveal>
		<HubShelf title="Favorites" {loading}>
			<div role="tablist" class="tabs tabs-box tabs-sm mb-3">
				<button
					role="tab"
					class="tab"
					class:tab-active={tab === 'albums'}
					aria-selected={tab === 'albums'}
					onclick={() => (tab = 'albums')}
				>
					Albums ({albums.length})
				</button>
				<button
					role="tab"
					class="tab"
					class:tab-active={tab === 'artists'}
					aria-selected={tab === 'artists'}
					onclick={() => (tab = 'artists')}
				>
					Artists ({artists.length})
				</button>
				<button
					role="tab"
					class="tab"
					class:tab-active={tab === 'tracks'}
					aria-selected={tab === 'tracks'}
					onclick={() => (tab = 'tracks')}
				>
					Tracks ({tracks.length})
				</button>
			</div>

			{#if tab === 'albums'}
				{#if albums.length > 0}
					<AlbumCarousel {albums} {onopen} />
				{:else}
					<p class="text-sm text-base-content/50">No favorite albums yet.</p>
				{/if}
			{:else if tab === 'artists'}
				{#if artists.length > 0}
					<HorizontalCarousel>
						{#each artists as artist (artist.id)}
							<div class="shrink-0 w-32 text-center">
								<div class="mx-auto h-28 w-28 overflow-hidden rounded-full">
									<ArtistImage
										mbid={artist.artist_mbid ?? artist.id}
										remoteUrl={artist.image_url}
										alt={artist.name}
										size="full"
										requestSize={250}
										rounded="full"
										className="h-full w-full"
									/>
								</div>
								<p class="text-sm font-medium mt-1 line-clamp-1">{artist.name}</p>
								<p class="text-xs opacity-60">
									{artist.album_count ?? 0} album{(artist.album_count ?? 0) !== 1 ? 's' : ''}
								</p>
							</div>
						{/each}
					</HorizontalCarousel>
				{:else}
					<p class="text-sm text-base-content/50">No favorite artists yet.</p>
				{/if}
			{:else if tracks.length > 0}
				<div class="max-h-72 overflow-y-auto rounded-lg">
					<table class="table table-sm">
						<thead>
							<tr>
								<th>#</th>
								<th>Title</th>
								<th>Artist</th>
								<th>Album</th>
								<th class="text-right">Duration</th>
							</tr>
						</thead>
						<tbody>
							{#each tracks as track, i (track.id)}
								<tr
									class="hover transition-all duration-200 hover:border-l-2 hover:border-l-primary hover:pl-1"
								>
									<td class="text-base-content/50">{i + 1}</td>
									<td class="font-medium">{track.title}</td>
									<td class="text-base-content/60">{track.artist_name}</td>
									<td class="text-base-content/60">{track.album_name}</td>
									<td class="text-right text-base-content/50"
										>{formatDurationSec(track.duration_secs ?? 0)}</td
									>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{:else}
				<p class="text-sm text-base-content/50">No favorite tracks yet.</p>
			{/if}
		</HubShelf>
	</div>
{/if}
