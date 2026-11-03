<script lang="ts">
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';

	interface MostPlayedArtist {
		id: string;
		name: string;
		image_url?: string | null;
		musicbrainz_id?: string | null;
		play_count?: number | null;
		album_count?: number;
	}

	interface MostPlayedAlbum {
		id: string;
		name: string;
		artist_name?: string;
		image_url?: string | null;
		musicbrainz_id?: string | null;
		play_count?: number | null;
	}

	interface Props {
		artists: MostPlayedArtist[];
		albums: MostPlayedAlbum[];
		onAlbumClick?: (album: MostPlayedAlbum) => void;
	}

	let { artists, albums, onAlbumClick }: Props = $props();
</script>

{#if artists.length === 0 && albums.length === 0}
	<p class="text-sm text-base-content/50">No listening data yet.</p>
{:else}
	<div class="grid grid-cols-1 gap-6 md:grid-cols-2">
		{#if artists.length > 0}
			<div class="space-y-2">
				<h3 class="text-sm font-semibold uppercase tracking-wider text-base-content/50">Artists</h3>
				<ol class="space-y-1">
					{#each artists as artist (artist.id)}
						<li
							class="flex items-center gap-3 rounded-lg px-2 py-1.5 transition-colors hover:bg-base-200/50"
						>
							<span class="w-5 text-right text-xs font-bold text-base-content/40"
								>{artists.indexOf(artist) + 1}</span
							>
							<div class="h-9 w-9 shrink-0 overflow-hidden rounded-full">
								<ArtistImage
									mbid={artist.musicbrainz_id ?? artist.id}
									remoteUrl={artist.image_url}
									alt={artist.name}
									size="xs"
									rounded="full"
									className="h-full w-full"
								/>
							</div>
							<div class="min-w-0 flex-1">
								<p class="truncate text-sm font-medium">{artist.name}</p>
								<p class="text-xs text-base-content/50">
									{artist.play_count ?? 0} plays{artist.album_count != null
										? `, ${artist.album_count} album${artist.album_count !== 1 ? 's' : ''}`
										: ''}
								</p>
							</div>
						</li>
					{/each}
				</ol>
			</div>
		{/if}

		{#if albums.length > 0}
			<div class="space-y-2">
				<h3 class="text-sm font-semibold uppercase tracking-wider text-base-content/50">Albums</h3>
				<ol class="space-y-1">
					{#each albums as album (album.id)}
						<li>
							<button
								class="flex w-full items-center gap-3 rounded-lg px-2 py-1.5 text-left transition-colors hover:bg-base-200/50"
								onclick={() => onAlbumClick?.(album)}
							>
								<span class="w-5 text-right text-xs font-bold text-base-content/40"
									>{albums.indexOf(album) + 1}</span
								>
								<div class="h-9 w-9 shrink-0 overflow-hidden rounded-md">
									<AlbumImage
										mbid={album.musicbrainz_id ?? album.id}
										customUrl={album.image_url}
										alt={album.name}
										size="xs"
										rounded="none"
										className="h-full w-full"
									/>
								</div>
								<div class="min-w-0 flex-1">
									<p class="truncate text-sm font-medium">{album.name}</p>
									<p class="truncate text-xs text-base-content/50">
										{album.artist_name ?? ''}{album.play_count != null
											? `, ${album.play_count} plays`
											: ''}
									</p>
								</div>
							</button>
						</li>
					{/each}
				</ol>
			</div>
		{/if}
	</div>
{/if}
