<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import LibraryFormatBadge from './LibraryFormatBadge.svelte';
	import LocalIdentityBadge from './LocalIdentityBadge.svelte';
	import NoFilesBadge from './NoFilesBadge.svelte';
	import type { AlbumView } from '$lib/features/library/queries/types';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { trackCountLabel } from '$lib/utils/trackCount';

	interface Props {
		album: AlbumView;
		/** What kind of release it is ("Single", "Live"), shown after the track count. */
		kind?: string;
	}

	let { album, kind }: Props = $props();
	let href = $derived(albumHref(album.release_group_mbid ?? album.id));
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
			mbid={album.release_group_mbid ?? ''}
			available={album.cover_available}
			artMissing={album.art_missing}
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
		{#if album.identity_state === 'local_only'}
			<LocalIdentityBadge
				state={album.identity_state}
				releaseMbid={album.release_mbid}
				subject="album"
				compact
				className="absolute left-2 top-2 z-10"
			/>
		{/if}
	</figure>

	<!-- One line for year and artist, cut letter by letter at the cover's
	     width; count and kind share a line and wrap only when short of room. -->
	<div class="space-y-1 px-3 pb-3">
		<CardTitle class="text-sm line-clamp-2 min-h-[2.5rem]">{album.title}</CardTitle>
		<p class="truncate text-xs leading-tight opacity-70">
			{#if album.year}{album.year}{:else}Unknown{/if}
			{#if album.artist_name}
				<span class="opacity-50 mx-1">•</span>{album.artist_name}
			{/if}
		</p>
		<p class="flex flex-wrap gap-x-1 text-[11px] leading-tight opacity-50">
			<span class="whitespace-nowrap">
				{trackCountLabel(album.track_count, album.expected_track_count)}
			</span>
			{#if kind}
				<span class="whitespace-nowrap"><span aria-hidden="true" class="mr-1">•</span>{kind}</span>
			{/if}
		</p>
	</div>
</Card>
