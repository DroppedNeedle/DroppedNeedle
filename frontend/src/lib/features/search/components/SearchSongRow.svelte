<!--
	One library song in a search: cover, title, "artist · album" (the album a
	link), duration, the play mark and the queue menu. The whole row plays on
	click, Enter or Space; the acid square only shows that.
-->
<script lang="ts">
	import { ListPlus, ListStart, Play } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ContextMenu from '$lib/ui/ContextMenu.svelte';
	import type { MenuItem } from '$lib/ui/ContextMenu.svelte';
	import NowPlayingIndicator from '$lib/features/player/components/NowPlayingIndicator.svelte';
	import {
		searchSongQueueItem,
		type SearchLibrarySong
	} from '$lib/features/search/queries/SearchV3Adapters';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { localAlbumHref } from '$lib/utils/entityRoutes';
	import { formatDurationSec } from '$lib/utils/formatting';

	interface Props {
		song: SearchLibrarySong;
	}

	let { song }: Props = $props();

	let playing = $derived(
		playerStore.isPlaying &&
			playerStore.currentQueueItem?.trackSourceId === song.id &&
			playerStore.currentQueueItem?.sourceType === 'local'
	);

	function play() {
		playerStore.playQueue([searchSongQueueItem(song)], 0, false);
	}

	const menuItems: MenuItem[] = [
		{
			label: 'Add to Queue',
			icon: ListPlus,
			onclick: () => {
				playerStore.addMultipleToQueue([searchSongQueueItem(song)]);
				toastStore.show({ message: `"${song.title}" was added to the queue`, type: 'info' });
			}
		},
		{
			label: 'Play Next',
			icon: ListStart,
			onclick: () => {
				playerStore.playMultipleNext([searchSongQueueItem(song)]);
				toastStore.show({ message: `"${song.title}" will play next`, type: 'info' });
			}
		}
	];
</script>

<div class="search-song group" class:is-playing={playing}>
	<button type="button" class="search-song__hit" aria-label="Play {song.title}" onclick={play}
	></button>
	<div class="relative size-12 shrink-0">
		<AlbumImage
			albumId={song.track.album_id}
			mbid={song.track.release_group_mbid ?? ''}
			available={song.track.cover_available}
			alt={song.track.album_title}
			size="full"
			requestSize={250}
			rounded="md"
			className="size-12"
		/>
		{#if playing}
			<div class="absolute inset-0 grid place-items-center rounded-md bg-black/55">
				<NowPlayingIndicator color="bg-white" />
			</div>
		{/if}
	</div>

	<div class="min-w-0 flex-1">
		<p class="search-song__title">
			{#if playing}<span class="sr-only">Now playing: </span>{/if}{song.title}
		</p>
		<p class="search-song__meta">
			{#if song.artist}{song.artist}<span class="mx-1.5" aria-hidden="true">·</span>{/if}<a
				href={localAlbumHref(song.track.album_id)}
				class="search-song__album">{song.track.album_title}</a
			>
		</p>
	</div>

	{#if song.track.duration_seconds != null}
		<span class="search-song__time">{formatDurationSec(song.track.duration_seconds)}</span>
	{/if}
	<span class="search-song__play" aria-hidden="true">
		<Play class="size-4 fill-current" />
	</span>
	<div class="search-song__menu">
		<ContextMenu items={menuItems} position="end" size="sm" label="More actions for {song.title}" />
	</div>
</div>

<style>
	.search-song {
		position: relative;
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
	.search-song.is-playing {
		background-color: color-mix(in oklab, var(--dn-title) 10%, transparent);
	}

	/* the row's one target, under its text; the album link and the menu sit above it */
	.search-song__hit {
		position: absolute;
		inset: 0;
		z-index: 1;
		cursor: pointer;
	}
	.search-song__hit:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: -2px;
	}

	.search-song__title {
		overflow: hidden;
		font-size: 0.9375rem;
		font-weight: 600;
		line-height: 1.3;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.is-playing .search-song__title {
		color: var(--dn-title);
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

	.search-song__album,
	.search-song__menu {
		position: relative;
		z-index: 2;
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

	/* the acid mark of the row's play; clicks fall through to the row */
	.search-song__play {
		display: grid;
		flex: none;
		place-items: center;
		width: 2.5rem;
		height: 1.75rem;
		margin-left: 0.75rem;
		border: 1px solid var(--dn-action-edge);
		border-radius: 0.375rem;
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		pointer-events: none;
	}

	/* a narrow phone gives the title the mark's room; the row still plays */
	@media (max-width: 379px) {
		.search-song__play {
			display: none;
		}
	}

	@media (max-width: 479px) {
		.search-song {
			gap: 0.625rem;
			padding-inline: 0.625rem 0.375rem;
		}
		.search-song__play {
			width: 2.25rem;
			margin-left: 0.25rem;
		}
	}
</style>
