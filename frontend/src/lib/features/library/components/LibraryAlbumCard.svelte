<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
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

<MediaTile
	title={album.title}
	titleAs="h2"
	{href}
	label="Open {album.title}"
	class="w-full shrink-0"
>
	{#snippet art()}
		<AlbumImage
			albumId={album.id}
			mbid={album.release_group_mbid ?? ''}
			available={album.cover_available}
			artMissing={album.art_missing}
			alt={album.title}
			size="full"
			requestSize={250}
			className="w-full h-full"
		/>
	{/snippet}
	<!-- One line for year and artist, cut letter by letter at the cover's
	     width; count and kind share a line and wrap only when short of room. -->
	{#snippet meta()}
		{#if album.year}{album.year}{:else}Unknown{/if}
		{#if album.artist_name}
			<span class="mr-1" aria-hidden="true">·</span>{album.artist_name}
		{/if}
	{/snippet}
	{#snippet footer()}
		<p class="flex flex-wrap gap-x-1 font-mono text-xs leading-tight text-muted">
			<span class="whitespace-nowrap">
				{trackCountLabel(album.track_count, album.expected_track_count)}
			</span>
			{#if kind}
				<span class="whitespace-nowrap"><span aria-hidden="true" class="mr-1">·</span>{kind}</span>
			{/if}
		</p>
	{/snippet}
	{#snippet overlay()}
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
	{/snippet}
</MediaTile>
