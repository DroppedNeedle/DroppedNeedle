<script lang="ts">
	import { v3 } from '$lib/api/v3/endpoint';
	import { Play, Shuffle, ListPlus, ListStart, ListMusic, Download } from 'lucide-svelte';
	import ContextMenu from '$lib/ui/ContextMenu.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import type { MenuItem } from '$lib/ui/ContextMenu.svelte';
	import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
	import {
		cardQuickPlay,
		cardQuickShuffle,
		cardAddToQueue,
		cardPlayNext,
		fetchAlbumQueueItems,
		type AlbumCardMeta
	} from '$lib/features/album/components/albumCardPlayback';
	import { openGlobalPlaylistModal } from '$lib/features/playlists/state/playlistModal.svelte';
	import { downloadAlbumArchive } from '$lib/features/album/components/downloadActions';
	import { getDownloadAccessQuery } from '$lib/features/album/queries/DownloadAccessQuery.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';

	const integrations = useIntegrationStatus();

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
		integrations.current.localfiles ||
			integrations.current.navidrome ||
			integrations.current.jellyfin
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
		if (integrations.current.localfiles && downloadAllowed) {
			items.push({
				label: 'Download Album',
				icon: Download,
				onclick: () =>
					void downloadAlbumArchive(
						v3('/api/v3/download/local/album/mbid/{mbid}', { path: { mbid } })
					)
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
			<IconButton
				variant="neutral"
				size="sm"
				icon={Shuffle}
				label="Shuffle {albumName}"
				loading={isShuffling}
				onclick={handleShuffle}
			/>
		{/if}
		<IconButton
			variant="primary"
			size="sm"
			icon={Play}
			label="Play {albumName}"
			loading={isPlaying}
			class="[&_svg]:fill-current"
			onclick={handlePlay}
		/>
	</div>
{/if}
