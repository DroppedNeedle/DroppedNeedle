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

	function isAlbum(_value: HomeAlbum | HomeArtist): _value is HomeAlbum {
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
	class={`card ${className} group ${href ? 'cursor-pointer' : 'cursor-default'}`}
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
					requestSize={250}
					rounded="none"
					className="w-full h-full"
					customUrl={(item as HomeAlbum).image_url || null}
				/>
			{:else}
				<ArtistImage
					mbid={item.mbid || ''}
					alt={item.name}
					size="full"
					requestSize={250}
					rounded="none"
					className="w-full h-full"
					lazy={false}
				/>
			{/if}
			<div
				class="absolute inset-0 bg-gradient-to-t from-black/80 via-black/20 to-transparent"
			></div>
			<div class="absolute left-3 top-3 flex items-center gap-2">
				<span class="badge badge-primary badge-lg font-bold">#{rank}</span>
				<span class="badge badge-ghost badge-sm">Most Popular</span>
				{#if item.in_library}
					<div class="badge badge-success">
						<Check class="h-3 w-3" />
						In Library
					</div>
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
				<h3 class="line-clamp-2 text-lg font-bold sm:text-xl">{item.name}</h3>
				{#if isAlbum(item) && item.artist_name}
					<p class="line-clamp-1 text-sm text-white/80">{item.artist_name}</p>
				{/if}
				{#if item.listen_count !== null && item.listen_count !== undefined}
					<p class="mt-1 text-sm text-white/60">🎧 {formatListenCount(item.listen_count)}</p>
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
		{#if itemType === 'album'}
			<figure class="relative aspect-square overflow-hidden">
				<AlbumImage
					mbid={item.mbid || ''}
					alt={item.name}
					size="md"
					rounded="none"
					className="w-full h-full"
					customUrl={(item as HomeAlbum).image_url || null}
				/>
				{#if item.in_library}
					<div class="badge badge-success badge-sm absolute left-1 top-1 z-20">
						<Check class="h-3 w-3" />
					</div>
				{/if}
				{#if item.mbid && item.in_library}
					<AlbumCardOverlay
						mbid={item.mbid}
						albumName={item.name}
						artistName={(item as HomeAlbum).artist_name || 'Unknown'}
						coverUrl={(item as HomeAlbum).image_url || null}
						size="sm"
					/>
				{/if}
				<div
					class="badge badge-sm absolute bottom-1 left-1 font-bold {variant === 'expanded' &&
					rank <= 3
						? 'badge-primary'
						: 'badge-neutral'}"
				>
					#{rank}
				</div>
				{#if !item.mbid}
					<button
						type="button"
						class="btn btn-ghost btn-xs btn-circle absolute bottom-1 right-1"
						title="Search album"
						onclick={handleSearchClick}
					>
						<Search class="h-3 w-3" />
					</button>
				{/if}
			</figure>
		{:else}
			<figure
				class={variant === 'overview'
					? 'relative flex justify-center pt-4'
					: 'relative aspect-square overflow-hidden'}
			>
				<ArtistImage
					mbid={item.mbid || ''}
					alt={item.name}
					size={variant === 'overview' ? 'md' : 'full'}
					rounded={variant === 'overview' ? undefined : 'none'}
					className={variant === 'overview' ? undefined : 'w-full h-full'}
					lazy={variant === 'overview' ? false : undefined}
				/>
				{#if item.in_library}
					<div class="badge badge-success badge-sm absolute right-1 top-1">
						<Check class="h-3 w-3" />
					</div>
				{/if}
				<div
					class="badge badge-sm absolute bottom-1 left-1 font-bold {variant === 'expanded' &&
					rank <= 3
						? 'badge-primary'
						: 'badge-neutral'}"
				>
					#{rank}
				</div>
				{#if !item.mbid}
					<button
						type="button"
						class="btn btn-ghost btn-xs btn-circle absolute bottom-1 right-1"
						title="Search artist"
						onclick={handleSearchClick}
					>
						<Search class="h-3 w-3" />
					</button>
				{/if}
			</figure>
		{/if}
		<div class={variant === 'expanded' ? 'card-body p-2 sm:p-3' : 'card-body p-2'}>
			<h3
				class={variant === 'expanded'
					? 'card-title line-clamp-1 text-xs sm:text-sm'
					: 'card-title line-clamp-1 text-xs'}
			>
				{item.name}
			</h3>
			{#if isAlbum(item) && item.artist_name}
				<p class="line-clamp-1 text-xs text-base-content/50">{item.artist_name}</p>
			{/if}
			{#if item.listen_count !== null && item.listen_count !== undefined}
				<p class="text-xs text-base-content/40">{formatListenCount(item.listen_count)}</p>
			{/if}
		</div>
	{/if}
</svelte:element>
