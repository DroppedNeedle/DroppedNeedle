<script lang="ts">
	import { resolve } from '$app/paths';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { onMount } from 'svelte';
	import { Disc3, Download } from 'lucide-svelte';
	import type { TopAlbum } from '$lib/types';
	import { colors } from '$lib/colors';
	import { libraryStore } from '$lib/stores/library';
	import { requestAlbum } from '$lib/utils/albumRequest';
	import AlbumImage from './AlbumImage.svelte';
	import LibraryBadge from './LibraryBadge.svelte';
	import LastFmPlaceholder from './LastFmPlaceholder.svelte';
	import { SvelteSet } from 'svelte/reactivity';

	interface Props {
		albums: TopAlbum[];
		loading?: boolean;
		configured?: boolean;
		source?: string;
	}

	let { albums, loading = false, configured = true, source = '' }: Props = $props();

	let requestingIds = new SvelteSet<string>();

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
			await requestAlbum(id, {
				artist: album.artist_name ?? undefined,
				album: album.title ?? undefined,
				year: album.year ?? undefined
			});
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
		<div class="bg-base-200 rounded-lg p-4 text-center flex-1 flex items-center justify-center">
			<div>
				<p class="text-base-content/70 text-sm">Connect a music service to see popular albums</p>
				<a href={resolve('/profile#scrobbling')} class="btn btn-primary btn-xs mt-2">Configure</a>
			</div>
		</div>
	{:else if albums.length === 0}
		<div class="bg-base-200 rounded-lg p-4 text-center flex-1 flex items-center justify-center">
			<p class="text-base-content/70 text-sm">No album data available</p>
		</div>
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
							<button
								type="button"
								class="btn btn-circle btn-sm opacity-0 group-hover:opacity-100 transition-all shrink-0 hover:scale-110 hover:brightness-110"
								style="background-color: {colors.accent}; border: none;"
								onclick={(e) => {
									e.stopPropagation();
									e.preventDefault();
									handleRequest(album);
								}}
								disabled={isRequesting(album)}
								aria-label="Request album"
							>
								{#if isRequesting(album)}
									<span
										class="loading loading-spinner loading-xs"
										style="color: {colors.secondary};"
									></span>
								{:else}
									<Download class="h-4 w-4" color={colors.secondary} strokeWidth={2.5} />
								{/if}
							</button>
						{/if}
					</a>
				{:else}
					<div
						class="flex items-center gap-3 p-2 rounded-lg transition-colors {source === 'lastfm'
							? 'opacity-75'
							: ''}"
					>
						{#if source === 'lastfm'}
							<LastFmPlaceholder />
						{:else}
							<div class="w-12 h-12 shrink-0 bg-base-200 rounded flex items-center justify-center">
								<Disc3 class="w-6 h-6 text-base-content/20" />
							</div>
						{/if}

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
