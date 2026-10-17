<script lang="ts">
	import type { Album, EnrichmentSource } from '$lib/types';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { libraryStore } from '$lib/stores/library';
	import { integrationStore } from '$lib/stores/integration';
	import { requestAlbum } from '$lib/queries/downloads/DownloadMutations.svelte';
	import { formatListenCount } from '$lib/utils/formatting';
	import { getListenTitle } from '$lib/utils/enrichment';
	import { Download, Music2 } from 'lucide-svelte';
	import AlbumImage from './AlbumImage.svelte';
	import LibraryBadge from './LibraryBadge.svelte';
	import AlbumCardOverlay from './AlbumCardOverlay.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		album: Album;
		enrichmentSource?: EnrichmentSource;
		onadded?: (() => void) | undefined;
		onremoved?: (() => void) | undefined;
		onenrichmentrequest?: (() => void) | undefined;
	}

	let {
		album = $bindable(),
		enrichmentSource = 'none',
		onadded = undefined,
		onremoved = undefined,
		onenrichmentrequest = undefined
	}: Props = $props();

	let listenTitle = $derived(getListenTitle(enrichmentSource, 'album'));

	let requesting = $state(false);

	const albumRequest = requestAlbum();

	let inLibrary = $derived(
		libraryStore.isInLibrary(album.musicbrainz_id) ||
			(!$libraryStore.initialized && album.in_library) ||
			false
	);
	let isRequested = $derived(
		!inLibrary && (album.requested || libraryStore.isRequested(album.musicbrainz_id))
	);
	let isLocalOnly = $derived(Boolean(album.local_id && album.local_id === album.musicbrainz_id));

	async function handleRequest(e: Event) {
		e.stopPropagation();
		requesting = true;
		try {
			const result = await albumRequest
				.mutateAsync({
					release_group_mbid: album.musicbrainz_id,
					artist_name: album.artist ?? undefined,
					album_title: album.title,
					year: album.year ?? undefined
				})
				.catch(() => null);
			if (result?.success) {
				onadded?.();
			}
		} finally {
			requesting = false;
		}
	}

	function handleDeleted() {
		album.in_library = false;
		album.requested = false;
		album = album;
		onremoved?.();
	}

	function getTypeBadgeClass(type: string): string {
		const lower = type.toLowerCase();
		if (lower.includes('ep')) return 'badge-warning';
		if (lower.includes('single')) return 'badge-info';
		if (lower.includes('compilation')) return 'badge-secondary';
		if (lower.includes('live')) return 'badge-error';
		return 'badge-ghost';
	}
</script>

<Card
	variant="raised"
	padding="none"
	class="group w-full shrink-0 transition hover:shadow-md motion-safe:hover:-translate-y-0.5"
>
	<a
		href={albumHref(album.musicbrainz_id)}
		onpointerenter={isLocalOnly ? undefined : onenrichmentrequest}
		onfocus={isLocalOnly ? undefined : onenrichmentrequest}
		class="block h-full relative z-0 transition-transform active:scale-95"
		aria-label="Open {album.title}"
	>
		<figure class="aspect-square overflow-hidden relative">
			<AlbumImage
				mbid={album.local_id ?? album.musicbrainz_id}
				customUrl={album.cover_url}
				remoteUrl={album.album_thumb_url ?? null}
				source={album.local_id ? 'local' : 'provider'}
				available={album.local_id ? (album.cover_available ?? false) : true}
				alt={album.title}
				size="full"
				requestSize={250}
				responsiveSizes="(max-width: 639px) calc(50vw - 2.5rem), (max-width: 1023px) calc(25vw - 2rem), 200px"
				rounded="none"
				className="w-full h-full"
			/>
			{#if inLibrary && !isLocalOnly}
				<AlbumCardOverlay
					mbid={album.musicbrainz_id}
					albumName={album.title}
					artistName={album.artist || 'Unknown'}
					coverUrl={album.cover_url ?? album.album_thumb_url ?? null}
				/>
			{/if}
		</figure>

		<div class="flex flex-col gap-2 p-3">
			<h2 class="card-title text-sm line-clamp-2 min-h-[2.5rem]">{album.title}</h2>
			<p class="text-xs opacity-70 line-clamp-1">
				{#if album.year}{album.year}{:else}Unknown{/if}
				{#if album.artist}
					<span class="opacity-50 mx-1">•</span>
					{album.artist}
				{/if}
			</p>

			{#if album.listen_count != null}
				<div class="flex items-center gap-1 mt-1">
					{#if enrichmentSource === 'lastfm'}
						<span class="badge badge-sm border-0 bg-lastfm/15 text-lastfm" title={listenTitle}>
							Last.fm {formatListenCount(album.listen_count, true)}
						</span>
					{:else if enrichmentSource === 'listenbrainz'}
						<span
							class="badge badge-sm border-0 bg-listenbrainz/15 text-listenbrainz"
							title={listenTitle}
						>
							LB {formatListenCount(album.listen_count, true)}
						</span>
					{:else}
						<span class="badge badge-sm badge-ghost" title={listenTitle}>
							<Music2 class="inline h-3 w-3" />
							{formatListenCount(album.listen_count, true)}
						</span>
					{/if}
				</div>
			{/if}
		</div>
	</a>

	<div class="absolute top-2 left-2 z-20 flex flex-col items-start gap-1">
		{#if album.type_info && album.type_info !== 'Album'}
			<span class="badge badge-sm {getTypeBadgeClass(album.type_info)}">{album.type_info}</span>
		{/if}
		{#if $integrationStore.download_client && !isLocalOnly}
			{#if inLibrary}
				<LibraryBadge
					status="library"
					musicbrainzId={album.musicbrainz_id}
					albumTitle={album.title}
					artistName={album.artist || 'Unknown'}
					ondeleted={handleDeleted}
				/>
			{:else if isRequested}
				<LibraryBadge
					status="requested"
					musicbrainzId={album.musicbrainz_id}
					albumTitle={album.title}
					artistName={album.artist || 'Unknown'}
					ondeleted={handleDeleted}
				/>
			{/if}
		{/if}
	</div>

	{#if $integrationStore.download_client && !inLibrary && !isRequested}
		<IconButton
			variant="accent"
			shape="square"
			icon={Download}
			label="Request {album.title}"
			loading={requesting}
			class="absolute bottom-2 left-2 z-20 opacity-0 transition-opacity duration-200 group-hover:opacity-100 focus-visible:opacity-100"
			onclick={handleRequest}
		/>
	{/if}
</Card>
