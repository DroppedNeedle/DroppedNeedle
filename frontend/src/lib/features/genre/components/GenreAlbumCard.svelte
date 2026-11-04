<script lang="ts">
	import { Check } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import AlbumCardOverlay from '$lib/features/album/components/AlbumCardOverlay.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import type { AlbumRef } from '$lib/features/discover/queries/types';

	interface Props {
		album: AlbumRef;
		showLibraryBadge?: boolean;
		onclick?: () => void;
		href?: string | null;
	}

	let { album, showLibraryBadge = false, onclick, href = null }: Props = $props();
</script>

<MediaTile
	title={album.title}
	href={href ?? undefined}
	onclick={href ? undefined : onclick}
	class="w-full"
>
	{#snippet art()}
		<AlbumImage
			mbid={album.musicbrainz_id || ''}
			alt={album.title}
			size="full"
			className="w-full h-full"
			customUrl={album.cover_url || null}
		/>
	{/snippet}
	{#snippet meta()}
		{album.artist_name || 'Unknown Artist'}
		{#if album.release_date}
			<span class="mr-1" aria-hidden="true">·</span>{album.release_date}
		{/if}
	{/snippet}
	{#snippet overlay()}
		{#if showLibraryBadge || album.in_library}
			<StatusChip tone="action" size="xs" icon={Check} class="absolute top-2 left-2 z-20">
				<span class="sr-only">In library</span>
			</StatusChip>
		{/if}
		{#if album.musicbrainz_id && album.in_library}
			<AlbumCardOverlay
				mbid={album.musicbrainz_id}
				albumName={album.title}
				artistName={album.artist_name || 'Unknown'}
				coverUrl={album.cover_url || null}
			/>
		{/if}
	{/snippet}
</MediaTile>
