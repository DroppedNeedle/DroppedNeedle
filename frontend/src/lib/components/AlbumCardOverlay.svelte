<script lang="ts">
	import { Play, Shuffle, ListPlus, ListStart, ListMusic, Download } from 'lucide-svelte';
	import ContextMenu from './ContextMenu.svelte';
	import type { MenuItem } from './ContextMenu.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import {
		cardQuickPlay,
		cardQuickShuffle,
		cardAddToQueue,
		cardPlayNext,
		fetchAlbumQueueItems,
		type AlbumCardMeta
	} from '$lib/utils/albumCardPlayback';
	import { openGlobalPlaylistModal } from '$lib/stores/playlistModal.svelte';
	import { downloadAlbumArchive } from '$lib/utils/downloadActions';
	import { getDownloadAccessQuery } from '$lib/queries/local/DownloadAccessQuery.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { API } from '$lib/constants';

	interface Props {
		mbid: string;
		albumName: string;
		artistName: string;
		coverUrl: string | null;
		artistId?: string;
		size?: 'sm' | 'md';
	}

	let { mbid, albumName, artistName, coverUrl, artistId, size = 'md' }: Props = $props();

	let isPlaying = $state(false);
	let isShuffling = $state(false);

	let hasPlaybackSource = $derived(
		$integrationStore.localfiles || $integrationStore.navidrome || $integrationStore.jellyfin
	);

	const downloadAccessQuery = getDownloadAccessQuery(() => authStore.user?.id);
	// Fail-open on a missing bit (loading / legacy responses), like the header button.
	const downloadAllowed = $derived(downloadAccessQuery.data?.allowed !== false);

	function getMeta(): AlbumCardMeta {
		return { mbid, albumName, artistName, coverUrl, artistId };
	}

	function getMenuItems(): MenuItem[] {
		const items: MenuItem[] = [
			{
				label: 'Add to Queue',
				icon: ListPlus,
				onclick: () => void cardAddToQueue(getMeta())
			},
			{
				label: 'Play Next',
				icon: ListStart,
				onclick: () => void cardPlayNext(getMeta())
			},
			{
				label: 'Add to Playlist',
				icon: ListMusic,
				onclick: async () => {
					const items = await fetchAlbumQueueItems(getMeta());
					if (items.length > 0) openGlobalPlaylistModal(items);
				}
			}
		];
		if ($integrationStore.localfiles && downloadAllowed) {
			items.push({
				label: 'Download Album',
				icon: Download,
				onclick: () => void downloadAlbumArchive(API.download.localAlbumByMbid(mbid))
			});
		}
		return items;
	}

	async function handlePlay(e: Event) {
		e.stopPropagation();
		e.preventDefault();
		if (isPlaying) return;
		isPlaying = true;
		try {
			await cardQuickPlay(getMeta());
		} finally {
			isPlaying = false;
		}
	}

	async function handleShuffle(e: Event) {
		e.stopPropagation();
		e.preventDefault();
		if (isShuffling) return;
		isShuffling = true;
		try {
			await cardQuickShuffle(getMeta());
		} finally {
			isShuffling = false;
		}
	}
</script>

{#if hasPlaybackSource}
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="absolute top-2 right-2 z-20"
		onclick={(e) => {
			e.stopPropagation();
			e.preventDefault();
		}}
		onkeydown={(e) => {
			if (e.key === 'Enter' || e.key === ' ') {
				e.stopPropagation();
				e.preventDefault();
			}
		}}
	>
		<div class="rounded-full bg-black/50 backdrop-blur-sm">
			<ContextMenu items={getMenuItems()} position="end" size="xs" />
		</div>
	</div>

	<div
		class="absolute bottom-2 right-2 z-20 flex items-center gap-1.5 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 transition-opacity"
	>
		{#if size === 'md'}
			<button
				class="btn btn-circle btn-sm btn-ghost bg-black/50 backdrop-blur-sm text-white shadow-md hover:bg-black/70"
				onclick={handleShuffle}
				aria-label="Shuffle {albumName}"
			>
				{#if isShuffling}
					<span class="loading loading-spinner loading-xs"></span>
				{:else}
					<Shuffle class="h-3.5 w-3.5" />
				{/if}
			</button>
		{/if}
		<button
			class="btn btn-circle btn-sm btn-primary shadow-md"
			onclick={handlePlay}
			aria-label="Play {albumName}"
		>
			{#if isPlaying}
				<span class="loading loading-spinner loading-xs"></span>
			{:else}
				<Play class="h-4 w-4 fill-current" />
			{/if}
		</button>
	</div>
{/if}
