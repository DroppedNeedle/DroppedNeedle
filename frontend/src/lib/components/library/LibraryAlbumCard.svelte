<script lang="ts">
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import LibraryFormatBadge from './LibraryFormatBadge.svelte';
	import LocalIdentityBadge from './LocalIdentityBadge.svelte';
	import NoFilesBadge from './NoFilesBadge.svelte';
	import type { LibraryAlbumSummary } from '$lib/types';
	import { albumHref } from '$lib/utils/entityRoutes';

	interface Props {
		album: LibraryAlbumSummary;
	}

	let { album }: Props = $props();
	let href = $derived(albumHref(album.musicbrainz_release_group_id ?? album.id));
</script>

<Card
	{href}
	variant="raised"
	padding="none"
	class="group relative w-full shrink-0 overflow-hidden"
	aria-label="Open {album.title}"
>
	<figure class="aspect-square overflow-hidden relative">
		<AlbumImage
			albumId={album.id}
			mbid={album.musicbrainz_release_group_id ?? ''}
			available={album.cover_available}
			alt={album.title}
			size="full"
			requestSize={250}
			rounded="none"
			className="w-full h-full"
		/>
		<div class="absolute top-2 right-2 z-10">
			{#if album.track_count === 0}
				<NoFilesBadge />
			{:else}
				<LibraryFormatBadge format={album.format} />
			{/if}
		</div>
		{#if album.album_identity_state === 'local_only'}
			<LocalIdentityBadge
				state={album.album_identity_state}
				subject="album"
				compact
				className="absolute left-2 top-2 z-10"
			/>
		{/if}
	</figure>

	<div class="space-y-2 px-3 pb-3">
		<CardTitle class="text-sm line-clamp-2 min-h-[2.5rem]">{album.title}</CardTitle>
		<p class="text-xs opacity-70 line-clamp-1">
			{#if album.year}{album.year}{:else}Unknown{/if}
			{#if album.artist_name}
				<span class="opacity-50 mx-1">•</span>{album.artist_name}
			{/if}
		</p>
		<p class="text-[11px] opacity-50">
			{album.track_count}
			{album.track_count === 1 ? 'track' : 'tracks'}
		</p>
	</div>
</Card>
