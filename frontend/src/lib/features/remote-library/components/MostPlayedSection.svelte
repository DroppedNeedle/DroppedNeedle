<script lang="ts">
	import './hub.css';
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
	<p class="text-sm text-muted">No listening data yet.</p>
{:else}
	<div class="mp">
		{#if artists.length > 0}
			<div>
				<h3 class="hub-sub mb-2 px-2">Artists</h3>
				<ol class="mp__list">
					{#each artists as artist, i (artist.id)}
						<li class="mp__row">
							<span class="mp__rank">{i + 1}</span>
							<div class="mp__pic mp__pic--round">
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
								<p class="truncate font-semibold">{artist.name}</p>
								<p class="mp__meta">
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
			<div>
				<h3 class="hub-sub mb-2 px-2">Albums</h3>
				<ol class="mp__list">
					{#each albums as album, i (album.id)}
						<li>
							<button class="mp__row mp__row--button" onclick={() => onAlbumClick?.(album)}>
								<span class="mp__rank">{i + 1}</span>
								<div class="mp__pic">
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
									<p class="truncate font-semibold">{album.name}</p>
									<p class="mp__meta truncate">
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

<style>
	.mp {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 22rem), 1fr));
		gap: 1.5rem;
	}
	.mp__list {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 16rem), 1fr));
		gap: 0.25rem 1rem;
	}
	.mp__row {
		display: flex;
		width: 100%;
		align-items: center;
		gap: 0.875rem;
		padding: 0.5rem;
		border-radius: 0.375rem;
		text-align: left;
	}
	.mp__row--button:hover {
		background: color-mix(in oklab, var(--dn-title) 7%, transparent);
	}
	.mp__rank {
		width: 2rem;
		flex: none;
		font-family: var(--font-display);
		font-size: 1.75rem;
		line-height: 1;
		text-align: right;
		color: var(--dn-title);
		font-synthesis: none;
	}
	.mp__pic {
		width: 2.75rem;
		height: 2.75rem;
		flex: none;
		overflow: hidden;
		border-radius: 0.25rem;
	}
	.mp__pic--round {
		border-radius: 999px;
	}
	.mp__meta {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.04em;
		color: var(--dn-muted);
	}
</style>
