<!--
	One song the library lacks, in a search: cover, title, "artist · album"
	(the album a link when MusicBrainz named its group), duration, and the
	Request action, or the REQUESTED chip while a request is open.
-->
<script lang="ts">
	import { Clock } from 'lucide-svelte';
	import TrackRequestButton from '$lib/features/downloads/components/TrackRequestButton.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import type { SearchRemoteSong } from '$lib/features/search/queries/SearchV3Adapters';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { formatDurationSec } from '$lib/utils/formatting';

	interface Props {
		song: SearchRemoteSong;
	}

	let { song }: Props = $props();

	const detail = $derived(song.remote_track);
</script>

<div class="search-song">
	<div class="size-12 shrink-0">
		<AlbumImage
			mbid={detail.release_group_mbid ?? ''}
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
				>{/if}{#if detail.album_title}{#if detail.release_group_mbid}<a
						href={albumHref(detail.release_group_mbid)}
						class="search-song__album">{detail.album_title}</a
					>{:else}{detail.album_title}{/if}{/if}
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
				albumMbid={detail.release_group_mbid ?? ''}
				albumTitle={detail.album_title}
				durationSeconds={detail.duration_seconds}
				artistMbid={detail.artist_mbid}
				releaseMbid={detail.release_mbid}
			/>
		{/if}
	</div>
</div>

<style>
	.search-song {
		display: flex;
		align-items: center;
		gap: 0.875rem;
		min-height: 4rem;
		padding: 0.5rem 0.75rem 0.5rem 0.875rem;
		transition: background-color 150ms;
	}
	.search-song:hover {
		background-color: var(--color-base-300);
	}

	.search-song__title {
		overflow: hidden;
		font-size: 0.9375rem;
		font-weight: 600;
		line-height: 1.3;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.search-song__meta {
		margin-top: 0.125rem;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--color-muted);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.search-song__album {
		border-radius: 0.125rem;
	}
	.search-song__album:hover {
		color: var(--color-base-content);
		text-decoration: underline;
	}

	.search-song__time {
		flex: none;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-variant-numeric: tabular-nums;
		color: var(--color-base-content);
	}

	.search-song__action {
		display: grid;
		flex: none;
		place-items: center;
		min-width: 2.5rem;
		margin-left: 0.75rem;
	}

	@media (max-width: 479px) {
		.search-song {
			gap: 0.625rem;
			padding-inline: 0.625rem 0.375rem;
		}
		.search-song__action {
			margin-left: 0.25rem;
		}
	}
</style>
