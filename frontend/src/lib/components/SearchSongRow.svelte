<script lang="ts">
	import { ListPlus, ListStart, Play } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import { searchSongQueueItem, type SearchSong } from '$lib/queries/search/SearchV3Adapters';
	import { playerStore } from '$lib/stores/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { localAlbumHref } from '$lib/utils/entityRoutes';
	import { formatDurationSec } from '$lib/utils/formatting';

	interface Props {
		song: SearchSong;
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

<div
	class="group flex cursor-pointer items-center gap-3 px-3 py-2 transition-colors focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary {playing
		? 'bg-accent/10'
		: 'hover:bg-base-200/50'}"
	onclick={play}
	onkeydown={(e) =>
		e.target === e.currentTarget &&
		(e.key === 'Enter' || e.key === ' ') &&
		(e.preventDefault(), play())}
	tabindex="0"
	role="button"
	aria-label="Play {song.title}"
>
	<div class="relative h-12 w-12 shrink-0">
		<AlbumImage
			albumId={song.track.album_id}
			mbid={song.track.release_group_mbid ?? ''}
			available={song.track.cover_available}
			alt={song.track.album_title}
			size="full"
			requestSize={250}
			rounded="md"
			className="h-12 w-12 ring-1 ring-base-content/10"
		/>
		<div
			class="absolute inset-0 flex items-center justify-center rounded-md bg-black/45 opacity-0 transition-opacity {playing
				? 'opacity-100'
				: 'group-hover:opacity-100'}"
		>
			{#if playing}
				<NowPlayingIndicator />
			{:else}
				<Play class="h-5 w-5 fill-current text-white" />
			{/if}
		</div>
	</div>

	<div class="min-w-0 flex-1">
		<div class="truncate text-sm font-semibold {playing ? 'text-accent' : ''}">
			{song.title}
		</div>
		<div class="truncate text-xs text-base-content/55">
			{#if song.artist}{song.artist}<span class="mx-1 text-base-content/35">·</span>{/if}<span
				class="text-base-content/35"
				><a
					href={localAlbumHref(song.track.album_id)}
					class="hover:underline"
					onclick={(e) => e.stopPropagation()}>{song.track.album_title}</a
				></span
			>
		</div>
	</div>

	<div class="flex shrink-0 items-center gap-2">
		{#if song.track.duration_seconds != null}
			<span class="text-xs tabular-nums text-base-content/40 {playing ? 'text-accent/60' : ''}">
				{formatDurationSec(song.track.duration_seconds)}
			</span>
		{/if}
		<div
			class="opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100"
		>
			<ContextMenu items={menuItems} position="end" size="xs" />
		</div>
	</div>
</div>
