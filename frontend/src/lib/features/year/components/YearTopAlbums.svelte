<!--
	Top albums after the No. 1: 2 to 5 large, 6 to 10 small, each with its
	rank and plays. An ordered list, so a screen reader hears the rank.
-->
<script lang="ts">
	import type { YearReview } from '$lib/features/year/queries/year.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { plays, rank } from './yearFormat';

	type Album = YearReview['top_albums'][number];

	interface Props {
		/** The whole top list, the No. 1 first. */
		albums: Album[];
	}

	let { albums }: Props = $props();

	const large = $derived(albums.slice(1, 5));
	const small = $derived(albums.slice(5, 10));
</script>

{#snippet tile(album: Album, place: number, big: boolean)}
	<li class="yr-albums__item" class:is-big={big}>
		<span class="yr-albums__rank" aria-hidden="true">{rank(place)}</span>
		<MediaTile title={album.name} href={albumHref(album.id)} class="min-w-0 flex-1">
			{#snippet art()}
				<AlbumImage
					mbid={album.release_group_mbid ?? ''}
					albumId={album.id}
					available={album.cover_available}
					artMissing={album.art_missing}
					alt={album.name}
					size="full"
					requestSize={big ? 500 : 250}
					className="h-full w-full"
				/>
			{/snippet}
			{#snippet meta()}
				{#if big}{album.artist_name} ·{/if}
				{plays(album.plays)}
			{/snippet}
		</MediaTile>
	</li>
{/snippet}

<section aria-label="Top albums">
	<SectionHead title="Top albums" />
	<ol class="yr-albums__large" start="2">
		{#each large as album, i (album.id)}
			{@render tile(album, i + 2, true)}
		{/each}
	</ol>
	{#if small.length > 0}
		<ol class="yr-albums__small" start="6">
			{#each small as album, i (album.id)}
				{@render tile(album, i + 6, false)}
			{/each}
		</ol>
	{/if}
</section>

<style>
	.yr-albums__large,
	.yr-albums__small {
		display: grid;
		gap: 1.5rem 1rem;
		list-style: none;
	}
	.yr-albums__large {
		grid-template-columns: repeat(2, minmax(0, 1fr));
	}
	.yr-albums__small {
		grid-template-columns: repeat(3, minmax(0, 1fr));
		margin-top: 2rem;
	}
	@media (min-width: 1024px) {
		.yr-albums__large {
			grid-template-columns: repeat(4, minmax(0, 1fr));
			gap: 2rem 1.5rem;
		}
		.yr-albums__small {
			grid-template-columns: repeat(5, minmax(0, 1fr));
		}
	}

	.yr-albums__item {
		display: flex;
		align-items: flex-start;
		gap: 0.5rem;
		min-width: 0;
	}
	.yr-albums__item.is-big {
		flex-direction: column;
	}
	.yr-albums__rank {
		font-family: var(--font-display);
		font-size: 1.25rem;
		line-height: 1;
		color: var(--dn-title);
		font-variant-numeric: tabular-nums;
		font-synthesis: none;
	}
	.yr-albums__item.is-big .yr-albums__rank {
		font-size: 2.75rem;
	}
</style>
