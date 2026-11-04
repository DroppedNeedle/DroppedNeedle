<!--
	One Weekly Exploration track, drawn like a shelf's album tile: the square
	cover with its length and the track preview on it, the title and artist
	under it, and Request when the album can be asked for.
-->
<script lang="ts">
	import type { WeeklyTrack } from '$lib/features/discover/queries/types';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import { albumHrefOrNull, artistHrefOrNull } from '$lib/utils/entityRoutes';
	import { libraryStore } from '$lib/stores/library';
	import AlbumRequestButton from '$lib/features/requests/components/AlbumRequestButton.svelte';
	import SampleButton from './SampleButton.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';

	interface Props {
		track: WeeklyTrack;
	}

	let { track }: Props = $props();

	const albumHref = $derived(albumHrefOrNull(track.release_group_mbid));
	const artistHref = $derived(artistHrefOrNull(track.artist_mbid));
	const albumMbid = $derived(track.release_group_mbid);
	const isRequested = $derived(albumMbid ? libraryStore.isRequested(albumMbid) : false);

	function formatDuration(ms: number | null): string {
		if (!ms) return '';
		const totalSec = Math.floor(ms / 1000);
		const min = Math.floor(totalSec / 60);
		const sec = totalSec % 60;
		return `${min}:${sec.toString().padStart(2, '0')}`;
	}
</script>

{#snippet cover()}
	{#if track.cover_url}
		<AlbumImage
			customUrl={track.cover_url}
			alt={track.album_name || track.title}
			size="full"
			rounded="none"
			className="h-full w-full"
		/>
	{:else}
		<div class="relative h-full w-full overflow-hidden">
			<CoverPlaceholder id={albumMbid} name={track.album_name || track.title} />
		</div>
	{/if}
	{#if track.duration_ms}
		<span class="duration">{formatDuration(track.duration_ms)}</span>
	{/if}
{/snippet}

<div class="weekly-card w-32 shrink-0 sm:w-36 md:w-44">
	<div class="relative">
		{#if albumHref}
			<a href={albumHref} class="art" aria-label={track.album_name || track.title}>
				{@render cover()}
			</a>
		{:else}
			<div class="art">{@render cover()}</div>
		{/if}
		<div class="absolute right-2 bottom-2 z-10">
			<SampleButton
				sampleKey={`track:${track.artist_name}|${track.title}`}
				artist={track.artist_name}
				title={track.title}
				kind="track"
				size="sm"
				albumMbid={track.release_group_mbid}
				artistMbid={track.artist_mbid}
				coverUrl={track.cover_url}
			/>
		</div>
	</div>

	<div class="px-1 pt-2.5">
		<h3 class="title">{track.title}</h3>
		{#if artistHref}
			<a href={artistHref} class="meta">{track.artist_name}</a>
		{:else}
			<p class="meta">{track.artist_name}</p>
		{/if}
		{#if track.album_name}
			{#if albumHref}
				<a href={albumHref} class="meta">{track.album_name}</a>
			{:else}
				<p class="meta">{track.album_name}</p>
			{/if}
		{/if}
	</div>

	{#if albumMbid && !isRequested}
		<div class="px-1 pt-2 pb-1">
			<AlbumRequestButton
				mbid={albumMbid}
				artistName={track.artist_name}
				albumName={track.album_name || track.title}
				artistMbid={track.artist_mbid ?? undefined}
				labelled
			/>
		</div>
	{/if}
</div>

<style>
	.art {
		position: relative;
		display: block;
		aspect-ratio: 1;
		overflow: hidden;
		border-radius: 0.5rem;
		background: var(--color-base-200);
		transition: translate 150ms;
	}
	@media (hover: hover) and (prefers-reduced-motion: no-preference) {
		a.art:hover {
			translate: 0 -1px;
		}
	}

	.duration {
		position: absolute;
		top: 0.5rem;
		left: 0.5rem;
		border-radius: 0.25rem;
		padding: 0.125rem 0.375rem;
		background: var(--dn-action);
		color: var(--dn-action-content);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		font-variant-numeric: tabular-nums;
	}

	.title {
		overflow: hidden;
		font-size: 0.875rem;
		font-weight: 700;
		line-height: 1.25;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.meta {
		display: block;
		margin-top: 0.25rem;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		line-height: 1.35;
		color: var(--color-muted);
		text-overflow: ellipsis;
		white-space: nowrap;
		transition: color 150ms;
	}
	a.meta:hover {
		color: var(--color-base-content);
	}
</style>
