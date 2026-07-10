<script lang="ts">
	import { Check, Search } from 'lucide-svelte';
	import type { HomeAlbum, HomeArtist } from '$lib/types';
	import { formatListenCount } from '$lib/utils/formatting';
	import AlbumImage from './AlbumImage.svelte';
	import ArtistImage from './ArtistImage.svelte';
	import AlbumCardOverlay from './AlbumCardOverlay.svelte';

	type ItemType = 'album' | 'artist';
	type TimeRangeCardVariant = 'featured' | 'overview' | 'expanded';

	interface Props {
		item: HomeAlbum | HomeArtist;
		itemType: ItemType;
		href?: string | null;
		rank: number;
		variant: TimeRangeCardVariant;
		className: string;
		onFallbackClick: (item: HomeAlbum | HomeArtist) => void;
	}

	let { item, itemType, href = null, rank, variant, className, onFallbackClick }: Props = $props();

	function isAlbum(value: HomeAlbum | HomeArtist): value is HomeAlbum {
		return itemType === 'album';
	}

	function handleCardClick() {
		onFallbackClick(item);
	}

	function handleCardKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter') {
			onFallbackClick(item);
		}
	}

	function handleSearchClick(event: Event) {
		event.stopPropagation();
		onFallbackClick(item);
	}
</script>

<svelte:element
	this={href ? 'a' : 'div'}
	href={href ?? undefined}
	class={variant === 'featured'
		? `group relative block overflow-hidden rounded-2xl border border-base-content/8 bg-base-200/50 ${className} ${href ? 'cursor-pointer' : 'cursor-default'}`
		: `group flex items-center gap-3 px-2 py-2.5 transition-colors hover:bg-base-content/4 ${className} ${href ? 'cursor-pointer' : 'cursor-default'}`}
	onclick={href ? undefined : handleCardClick}
	onkeydown={href ? undefined : handleCardKeydown}
	role={href ? undefined : 'button'}
	tabindex={href ? undefined : 0}
>
	{#if variant === 'featured'}
		<figure class="relative aspect-square w-full">
			{#if itemType === 'album'}
				<AlbumImage
					mbid={item.mbid || ''}
					alt={item.name}
					size="xl"
					rounded="none"
					className="w-full h-full"
					customUrl={(item as HomeAlbum).image_url || null}
				/>
			{:else}
				<ArtistImage
					mbid={item.mbid || ''}
					alt={item.name}
					size="full"
					rounded="none"
					className="w-full h-full"
					lazy={false}
				/>
			{/if}
			<div
				class="absolute inset-0 bg-gradient-to-t from-black/80 via-black/20 to-transparent"
			></div>
			<div class="absolute left-3 top-3 flex flex-wrap items-center gap-1.5">
				<span
					class="rounded-full bg-primary px-2.5 py-1 font-mono text-[0.65rem] font-bold tabular-nums text-primary-content"
				>
					#{rank}
				</span>
				<span
					class="rounded-full border border-white/15 bg-black/40 px-2.5 py-1 font-mono text-[0.6rem] font-bold uppercase tracking-[0.14em] text-white/75 backdrop-blur-sm"
				>
					Most popular
				</span>
				{#if item.in_library}
					<span
						class="flex items-center gap-1 rounded-full border border-accent/40 bg-accent/20 px-2.5 py-1 font-mono text-[0.6rem] font-bold uppercase tracking-[0.14em] text-accent backdrop-blur-sm"
					>
						<Check class="h-3 w-3" />
						In library
					</span>
				{/if}
			</div>
			{#if isAlbum(item) && item.mbid && item.in_library}
				<AlbumCardOverlay
					mbid={item.mbid}
					albumName={item.name}
					artistName={(item as HomeAlbum).artist_name || 'Unknown'}
					coverUrl={(item as HomeAlbum).image_url || null}
				/>
			{/if}
			<div class="absolute inset-x-0 bottom-0 p-4 text-white">
				<h3 class="line-clamp-2 font-display text-lg font-bold sm:text-xl">{item.name}</h3>
				{#if isAlbum(item) && item.artist_name}
					<p class="line-clamp-1 text-sm text-white/80">{item.artist_name}</p>
				{/if}
				{#if item.listen_count !== null && item.listen_count !== undefined}
					<p class="mt-1 font-mono text-xs tabular-nums text-white/60">
						{formatListenCount(item.listen_count)}
					</p>
				{/if}
			</div>
			{#if !item.mbid}
				<button
					type="button"
					class="btn btn-ghost btn-xs btn-circle absolute bottom-3 right-3 text-white"
					title={itemType === 'album' ? 'Search album' : 'Search artist'}
					onclick={handleSearchClick}
				>
					<Search class="h-3 w-3" />
				</button>
			{/if}
		</figure>
	{:else}
		<span
			class="w-7 shrink-0 text-right font-mono text-xs font-bold tabular-nums {rank <= 3
				? 'text-primary'
				: 'text-base-content/35'}"
		>
			{rank}
		</span>
		<figure
			class="relative h-11 w-11 shrink-0 overflow-hidden {itemType === 'artist'
				? 'rounded-full'
				: 'rounded-lg'}"
		>
			{#if itemType === 'album'}
				<AlbumImage
					mbid={item.mbid || ''}
					alt={item.name}
					size="md"
					rounded="none"
					className="w-full h-full"
					customUrl={(item as HomeAlbum).image_url || null}
				/>
				{#if item.mbid && item.in_library}
					<AlbumCardOverlay
						mbid={item.mbid}
						albumName={item.name}
						artistName={(item as HomeAlbum).artist_name || 'Unknown'}
						coverUrl={(item as HomeAlbum).image_url || null}
						size="sm"
					/>
				{/if}
			{:else}
				<ArtistImage
					mbid={item.mbid || ''}
					alt={item.name}
					size="full"
					rounded="none"
					className="w-full h-full"
				/>
			{/if}
		</figure>
		<div class="min-w-0 flex-1">
			<h3 class="truncate text-sm font-medium">{item.name}</h3>
			{#if isAlbum(item) && item.artist_name}
				<p class="truncate text-xs text-base-content/45">{item.artist_name}</p>
			{/if}
		</div>
		{#if item.in_library}
			<span
				class="flex h-5 w-5 shrink-0 items-center justify-center rounded-full border border-accent/40 bg-accent/15 text-accent"
				title="In library"
			>
				<Check class="h-3 w-3" />
			</span>
		{/if}
		{#if item.listen_count !== null && item.listen_count !== undefined}
			<span class="shrink-0 font-mono text-xs tabular-nums text-base-content/45">
				{formatListenCount(item.listen_count)}
			</span>
		{/if}
		{#if !item.mbid}
			<button
				type="button"
				class="btn btn-ghost btn-xs btn-circle shrink-0"
				title={itemType === 'album' ? 'Search album' : 'Search artist'}
				onclick={handleSearchClick}
			>
				<Search class="h-3 w-3" />
			</button>
		{/if}
	{/if}
</svelte:element>
