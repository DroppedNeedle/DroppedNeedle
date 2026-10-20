<script lang="ts">
	import { albumHref } from '$lib/utils/entityRoutes';
	import { onMount } from 'svelte';
	import { Download } from 'lucide-svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import type { TopAlbum } from '$lib/types';
	import { libraryStore } from '$lib/stores/library';
	import { requestAlbum } from '$lib/queries/downloads/DownloadMutations.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import AlbumImage from './AlbumImage.svelte';
	import LibraryBadge from './LibraryBadge.svelte';
	import { SvelteSet } from 'svelte/reactivity';

	interface Props {
		albums: TopAlbum[];
		loading?: boolean;
		configured?: boolean;
		source?: string;
	}

	let { albums, loading = false, configured = true, source = '' }: Props = $props();

	let requestingIds = new SvelteSet<string>();

	const topAlbumRequest = requestAlbum();

	let libraryMbids = new SvelteSet<string>();
	let requestedMbids = new SvelteSet<string>();
	let storeInitialized = $state(false);

	onMount(() => {
		const unsubscribe = libraryStore.subscribe((state) => {
			libraryMbids = new SvelteSet(state.mbidSet);
			requestedMbids = new SvelteSet(state.requestedSet);
			storeInitialized = state.initialized;
		});
		return unsubscribe;
	});

	function isInLibrary(album: TopAlbum): boolean {
		const mbid = album.release_group_mbid?.toLowerCase();
		if (storeInitialized) return mbid ? libraryMbids.has(mbid) : false;
		return album.in_library || (mbid ? libraryMbids.has(mbid) : false);
	}

	function isRequested(album: TopAlbum): boolean {
		if (album.requested) return true;
		const mbid = album.release_group_mbid?.toLowerCase();
		if (!mbid) return false;
		return requestedMbids.has(mbid) && !libraryMbids.has(mbid);
	}

	function isRequesting(album: TopAlbum): boolean {
		return album.release_group_mbid ? requestingIds.has(album.release_group_mbid) : false;
	}

	async function handleRequest(album: TopAlbum) {
		if (!album.release_group_mbid) return;

		const id = album.release_group_mbid;
		requestingIds.add(id);

		try {
			await topAlbumRequest
				.mutateAsync({
					release_group_mbid: id,
					artist_name: album.artist_name ?? undefined,
					album_title: album.title ?? undefined,
					year: album.year ?? undefined
				})
				.catch(() => null);
		} finally {
			requestingIds.delete(id);
		}
	}
</script>

<div class="flex flex-col min-w-0">
	<h3 class="text-lg font-semibold mb-3">Popular Albums</h3>

	{#if loading}
		<div class="space-y-2">
			{#each Array(10) as _, i (`skeleton-${i}`)}
				<div class="flex items-center gap-3 p-2">
					<div class="skeleton w-12 h-12 rounded"></div>
					<div class="flex-1">
						<div class="skeleton h-4 w-3/4 mb-1"></div>
						<div class="skeleton h-3 w-1/2"></div>
					</div>
				</div>
			{/each}
		</div>
	{:else if !configured}
		<Card padding="md" class="flex-1 text-center">
			<div class="my-auto">
				<p class="text-base-content/70 text-sm">Connect a music service to see popular albums</p>
				<Button variant="primary" size="xs" href={withBasePath('/profile#scrobbling')} class="mt-2">
					Configure
				</Button>
			</div>
		</Card>
	{:else if albums.length === 0}
		<Card padding="md" class="flex-1 text-center">
			<p class="my-auto text-base-content/70 text-sm">No album data available</p>
		</Card>
	{:else}
		<div class="space-y-1">
			{#each albums as album, i (album.release_group_mbid || `album-${i}`)}
				{#if album.release_group_mbid}
					<a
						href={albumHref(album.release_group_mbid)}
						class="flex items-center gap-3 p-2 rounded-lg hover:bg-base-200 cursor-pointer transition-colors group"
					>
						<div class="w-12 h-12 shrink-0 relative">
							<AlbumImage
								mbid={album.release_group_mbid}
								alt={album.title}
								size="full"
								requestSize={250}
								className="w-12 h-12 rounded"
							/>
							{#if isInLibrary(album)}
								<LibraryBadge
									status="library"
									musicbrainzId={album.release_group_mbid}
									albumTitle={album.title}
									artistName={album.artist_name || 'Unknown'}
									size="sm"
									positioning="absolute -bottom-1 -right-1"
								/>
							{:else if isRequested(album)}
								<LibraryBadge
									status="requested"
									musicbrainzId={album.release_group_mbid}
									albumTitle={album.title}
									artistName={album.artist_name || 'Unknown'}
									size="sm"
									positioning="absolute -bottom-1 -right-1"
								/>
							{/if}
						</div>

						<div class="flex-1 min-w-0">
							<p class="font-medium text-sm truncate">{album.title}</p>
							<p class="text-xs text-base-content/50 truncate">
								{#if album.year}{album.year}{/if}
								{#if album.year && album.listen_count}<span class="mx-1">•</span>{/if}
								{#if album.listen_count}
									{album.listen_count.toLocaleString()} plays
								{/if}
							</p>
						</div>

						{#if !isInLibrary(album) && !isRequested(album)}
							<IconButton
								variant="accent"
								size="sm"
								icon={Download}
								label="Request album"
								class="shrink-0 opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
								loading={isRequesting(album)}
								onclick={(e: MouseEvent) => {
									e.stopPropagation();
									e.preventDefault();
									handleRequest(album);
								}}
							/>
						{/if}
					</a>
				{:else}
					<div
						class="flex items-center gap-3 p-2 rounded-lg transition-colors {source === 'lastfm'
							? 'opacity-75'
							: ''}"
					>
						<div class="relative w-12 h-12 shrink-0 overflow-hidden rounded">
							<CoverPlaceholder name={album.title} />
						</div>

						<div class="flex-1 min-w-0">
							<p class="font-medium text-sm truncate">{album.title}</p>
							<p class="text-xs text-base-content/50 truncate">
								{#if album.listen_count}
									{album.listen_count.toLocaleString()} plays
								{/if}
							</p>
						</div>
					</div>
				{/if}
			{/each}
		</div>
	{/if}
</div>
