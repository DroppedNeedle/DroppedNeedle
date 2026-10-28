<script lang="ts">
	import { Check } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import AlbumCardOverlay from '$lib/components/AlbumCardOverlay.svelte';
	import Card from '$lib/ui/Card.svelte';
	import type { AlbumRef } from '$lib/queries/discover/types';

	interface Props {
		album: AlbumRef;
		showLibraryBadge?: boolean;
		onclick?: () => void;
		href?: string | null;
	}

	let { album, showLibraryBadge = false, onclick, href = null }: Props = $props();
</script>

<Card
	padding="none"
	href={href ?? undefined}
	onclick={href ? undefined : onclick}
	onkeydown={href || !onclick
		? undefined
		: (e: KeyboardEvent) => {
				if (e.key === 'Enter' || e.key === ' ') {
					e.preventDefault();
					onclick();
				}
			}}
	role={href || !onclick ? undefined : 'button'}
	tabindex={href || !onclick ? undefined : 0}
	class="group {href
		? ''
		: onclick
			? 'cursor-pointer transition-colors hover:bg-base-300 focus-visible:outline-2 focus-visible:outline-primary'
			: 'cursor-default'}"
>
	<figure class="aspect-square overflow-hidden relative rounded-t-2xl">
		<AlbumImage
			mbid={album.musicbrainz_id || ''}
			alt={album.title}
			size="md"
			rounded="none"
			className="w-full h-full"
			customUrl={album.cover_url || null}
		/>
		{#if showLibraryBadge || album.in_library}
			<div class="absolute top-2 left-2 z-20 badge badge-success badge-sm gap-1 opacity-90">
				<Check class="w-3 h-3" />
			</div>
		{/if}
		{#if album.musicbrainz_id && album.in_library}
			<AlbumCardOverlay
				mbid={album.musicbrainz_id}
				albumName={album.title}
				artistName={album.artist_name || 'Unknown'}
				coverUrl={album.cover_url || null}
			/>
		{/if}
	</figure>
	<div class="flex flex-col gap-2 px-3 pt-1 pb-3">
		<h3 class="font-semibold text-sm line-clamp-1">{album.title}</h3>
		<p class="text-xs text-base-content/50 line-clamp-1">
			{album.artist_name || 'Unknown Artist'}
			{#if album.release_date}
				· {album.release_date}
			{/if}
		</p>
	</div>
</Card>
