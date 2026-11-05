<!--
	One album result: the Library album tile with search's chips and actions.
	Type and library state sit on the cover's top left, the album menu and
	Play (library albums) on its right, Request (others) on its bottom left;
	the listen count fills in under the title once it is fetched.
-->
<script lang="ts">
	import { Download } from 'lucide-svelte';
	import type { Album, EnrichmentSource } from '$lib/features/album/queries/types';
	import AlbumCardOverlay from '$lib/features/album/components/AlbumCardOverlay.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import SearchLibraryChip from './SearchLibraryChip.svelte';
	import SearchListens from './SearchListens.svelte';
	import {
		albumInLibrary,
		albumRequested,
		createSearchAlbumRequest,
		isLocalOnlyAlbum
	} from './searchAlbum.svelte';

	interface Props {
		album: Album;
		enrichmentSource?: EnrichmentSource;
		onenrichmentrequest?: () => void;
		/** Pass false for the first cards on screen. */
		lazy?: boolean;
	}

	let { album, enrichmentSource = null, onenrichmentrequest, lazy = true }: Props = $props();

	// The removal dialog answers here; the row itself is the query's.
	let removed = $state(false);
	const request = createSearchAlbumRequest();

	const localOnly = $derived(isLocalOnlyAlbum(album));
	const inLibrary = $derived(albumInLibrary(album, removed));
	const requested = $derived(albumRequested(album, removed));
</script>

<MediaTile
	title={album.title}
	titleAs="h3"
	href={albumHref(album.musicbrainz_id)}
	label="Open {album.title}"
	onintent={localOnly ? undefined : onenrichmentrequest}
	class="w-full shrink-0"
>
	{#snippet art()}
		<AlbumImage
			albumId={localOnly ? (album.local_id ?? undefined) : undefined}
			mbid={localOnly ? '' : album.musicbrainz_id}
			customUrl={album.cover_url}
			remoteUrl={album.album_thumb_url ?? null}
			artMissing={album.art_missing}
			alt={album.title}
			size="full"
			requestSize={250}
			{lazy}
			responsiveSizes="(max-width: 639px) 9rem, 200px"
			className="w-full h-full"
		/>
	{/snippet}
	{#snippet meta()}
		{#if album.year}{album.year}{:else}Unknown{/if}
		{#if album.artist}
			<span class="mr-1" aria-hidden="true">·</span>{album.artist}
		{/if}
	{/snippet}
	{#snippet footer()}
		{#if album.listen_count != null}
			<SearchListens count={album.listen_count} source={enrichmentSource} kind="album" />
		{/if}
	{/snippet}
	{#snippet overlay()}
		{#if inLibrary && !localOnly}
			<AlbumCardOverlay
				mbid={album.musicbrainz_id}
				albumName={album.title}
				artistName={album.artist || 'Unknown'}
				coverUrl={album.cover_url ?? album.album_thumb_url ?? null}
			/>
		{/if}
		<div class="absolute top-2 left-2 z-20 flex flex-col items-start gap-1">
			{#if album.type_info && album.type_info !== 'Album'}
				<StatusChip size="xs">{album.type_info}</StatusChip>
			{/if}
			{#if !localOnly && (inLibrary || requested)}
				<SearchLibraryChip
					status={inLibrary ? 'library' : 'requested'}
					musicbrainzId={album.musicbrainz_id}
					albumTitle={album.title}
					artistName={album.artist || 'Unknown'}
					ondeleted={() => (removed = true)}
				/>
			{/if}
		</div>
		{#if !inLibrary && !requested}
			<IconButton
				variant="primary"
				shape="square"
				size="sm"
				icon={Download}
				label="Request {album.title}"
				loading={request.pending}
				class="absolute bottom-2 left-2 z-20 opacity-0 transition-opacity duration-150 group-hover:opacity-100 group-focus-within:opacity-100 focus-visible:opacity-100"
				onclick={(event) => {
					event.stopPropagation();
					void request.request(album);
				}}
			/>
		{/if}
	{/snippet}
</MediaTile>
