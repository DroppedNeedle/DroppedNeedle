<!--
	Continue Listening: the last album played, big, with a button that plays
	it from the top, and up to eight more played before it. Every cover opens
	the album dialog.
-->
<script lang="ts">
	import './hub.css';
	import { CirclePlay, Play } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import VinylSleeve from '$lib/ui/VinylSleeve.svelte';
	import type { RemoteAlbum } from '$lib/features/remote-library/queries/types';

	interface Props {
		albums: RemoteAlbum[];
		onAlbumClick: (album: RemoteAlbum) => void;
		/** Plays the album from its first track. */
		onplay: (album: RemoteAlbum) => Promise<void>;
	}

	let { albums, onAlbumClick, onplay }: Props = $props();

	const hero = $derived(albums[0] ?? null);
	const thumbnails = $derived(albums.slice(1, 9));
	let starting = $state(false);

	async function play() {
		if (!hero || starting) return;
		starting = true;
		try {
			await onplay(hero);
		} finally {
			starting = false;
		}
	}
</script>

{#if hero}
	<section class="cl">
		<SectionHead title="Continue Listening" icon={CirclePlay} />
		<div class="hub-panel cl__panel">
			<button
				type="button"
				class="cl__cover"
				aria-label="Open {hero.title} by {hero.artist_name}"
				onclick={() => onAlbumClick(hero)}
			>
				<VinylSleeve class="w-full">
					<AlbumImage
						mbid={hero.release_group_mbid ?? hero.id}
						customUrl={hero.image_url}
						alt={hero.title}
						size="full"
						requestSize={500}
						rounded="none"
						priority
						className="h-full w-full"
					/>
				</VinylSleeve>
			</button>

			<div class="cl__info">
				<h3 class="cl__title">{hero.title}</h3>
				<p class="cl__artist">{hero.artist_name}</p>
				{#if hero.year}<p class="cl__year">{hero.year}</p>{/if}
				<Button
					variant="primary"
					size="display-sm"
					icon={Play}
					loading={starting}
					class="cl__play"
					aria-label="Continue listening: play {hero.title}"
					onclick={play}
				>
					Continue Listening
				</Button>
			</div>

			{#if thumbnails.length > 0}
				<div class="cl__thumbs" data-lazy-strip>
					{#each thumbnails as album (album.id)}
						<button
							type="button"
							class="cl__thumb"
							aria-label="Open {album.title} by {album.artist_name}"
							title="{album.title} - {album.artist_name}"
							onclick={() => onAlbumClick(album)}
						>
							<AlbumImage
								mbid={album.release_group_mbid ?? album.id}
								customUrl={album.image_url}
								alt={album.title}
								size="full"
								requestSize={250}
								rounded="none"
								className="h-full w-full"
							/>
						</button>
					{/each}
				</div>
			{/if}
		</div>
	</section>
{/if}

<style>
	.cl {
		display: flex;
		flex-direction: column;
		container: cl / inline-size;
	}
	.cl__panel {
		flex: 1;
		display: grid;
		grid-template-columns: 17rem minmax(0, 1fr) auto;
		align-items: center;
		gap: 1.75rem;
		padding: 1.25rem 1.5rem;
	}

	.cl__cover {
		display: block;
		width: 17rem;
		border-radius: 0.25rem;
		transition: transform 200ms var(--ease-spring);
	}
	.cl__thumb {
		display: block;
		aspect-ratio: 1;
		overflow: hidden;
		border-radius: 0.25rem;
		transition: transform 200ms var(--ease-spring);
	}
	.cl__cover:hover,
	.cl__thumb:hover {
		transform: translateY(-2px);
	}
	.cl__cover:focus-visible,
	.cl__thumb:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 3px;
	}

	.cl__info {
		display: flex;
		min-width: 0;
		flex-direction: column;
		align-items: flex-start;
	}
	.cl__title {
		display: -webkit-box;
		overflow: hidden;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		font-family: var(--font-display);
		font-size: 2.75rem;
		font-weight: 400;
		line-height: 0.95;
		text-transform: uppercase;
		font-synthesis: none;
		overflow-wrap: anywhere;
	}
	.cl__artist {
		margin-top: 0.5rem;
		font-family: var(--font-mono);
		font-size: 0.9375rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	.cl__year {
		margin-top: 0.125rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.14em;
		color: var(--dn-muted);
	}
	.cl__info :global(.cl__play) {
		margin-top: 1.25rem;
	}

	.cl__thumbs {
		display: grid;
		grid-template-columns: repeat(4, 5.75rem);
		gap: 0.625rem;
	}

	/* paired beside Now Playing: the thumbnails take their own row */
	@container cl (max-width: 46rem) {
		.cl__panel {
			grid-template-columns: 13rem minmax(0, 1fr);
			gap: 1.25rem;
			padding: 1rem;
		}
		.cl__cover {
			width: 13rem;
		}
		.cl__title {
			font-size: 2.25rem;
		}
		.cl__thumbs {
			grid-column: 1 / -1;
			grid-template-columns: repeat(8, minmax(0, 1fr));
		}
	}

	@container cl (max-width: 30rem) {
		.cl__panel {
			grid-template-columns: 8.5rem minmax(0, 1fr);
			grid-template-rows: auto auto 1fr;
			gap: 0.25rem 0.875rem;
		}
		.cl__cover {
			width: 8.5rem;
		}
		.cl__title {
			font-size: 1.625rem;
		}
		.cl__artist {
			font-size: 0.75rem;
		}
		.cl__info {
			display: contents;
		}
		.cl__cover {
			grid-row: 1 / 4;
			align-self: start;
		}
		.cl__title {
			align-self: end;
		}
		.cl__artist {
			margin-top: 0.25rem;
		}
		.cl__title,
		.cl__artist,
		.cl__year {
			grid-column: 2;
		}
		.cl__info :global(.cl__play) {
			grid-column: 1 / -1;
			margin-top: 0.75rem;
		}
		.cl__thumbs {
			grid-template-columns: repeat(4, minmax(0, 1fr));
			margin-top: 0.5rem;
		}
	}
</style>
