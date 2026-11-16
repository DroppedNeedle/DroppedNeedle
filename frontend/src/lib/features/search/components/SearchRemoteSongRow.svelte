<!--
	One song the library lacks, in a search: cover, title, "artist · album"
	(the album a link), duration, and the Request action, or the REQUESTED
	chip while a request is open. Shares searchSong.css with SearchSongRow.
-->
<script lang="ts">
	import { Clock } from 'lucide-svelte';
	import TrackRequestButton from '$lib/features/downloads/components/TrackRequestButton.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import type { SearchRemoteSong } from '$lib/features/search/queries/SearchV3Adapters';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { formatDurationSec } from '$lib/utils/formatting';
	import './searchSong.css';

	interface Props {
		song: SearchRemoteSong;
	}

	let { song }: Props = $props();

	const detail = $derived(song.remote_track);
</script>

<div class="search-song">
	<div class="size-12 shrink-0">
		<AlbumImage
			mbid={detail.release_group_mbid}
			alt={detail.album_title ?? song.title}
			size="full"
			requestSize={250}
			rounded="md"
			className="size-12"
		/>
	</div>

	<div class="min-w-0 flex-1">
		<p class="search-song__title">{song.title}</p>
		<p class="search-song__meta">
			{#if song.artist}{song.artist}{/if}{#if song.artist && detail.album_title}<span
					class="mx-1.5"
					aria-hidden="true">·</span
				>{/if}{#if detail.album_title}<a
					href={albumHref(detail.release_group_mbid)}
					class="search-song__album">{detail.album_title}</a
				>{/if}
		</p>
	</div>

	{#if detail.duration_seconds != null}
		<span class="search-song__time">{formatDurationSec(detail.duration_seconds)}</span>
	{/if}
	<div class="search-song__action">
		{#if song.requested}
			<StatusChip tone="warning" size="xs" icon={Clock}>Requested</StatusChip>
		{:else}
			<TrackRequestButton
				recordingMbid={song.musicbrainz_id}
				trackTitle={song.title}
				artistName={song.artist ?? ''}
				albumMbid={detail.release_group_mbid}
				albumTitle={detail.album_title}
				durationSeconds={detail.duration_seconds}
				artistMbid={detail.artist_mbid}
				releaseMbid={detail.release_mbid}
			/>
		{/if}
	</div>
</div>

<style>
	.search-song__action {
		display: grid;
		flex: none;
		place-items: center;
		min-width: 2.5rem;
		margin-left: 0.75rem;
	}

	@media (max-width: 479px) {
		.search-song__action {
			margin-left: 0.25rem;
		}
	}
</style>
