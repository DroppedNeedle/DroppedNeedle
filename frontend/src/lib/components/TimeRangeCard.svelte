<script lang="ts">
	import { Check, Headphones, Search } from 'lucide-svelte';
	import type { AlbumRef, ArtistRef } from '$lib/queries/discover/types';
	import { formatListenCount } from '$lib/utils/formatting';
	import AlbumImage from './AlbumImage.svelte';
	import ArtistImage from './ArtistImage.svelte';
	import AlbumCardOverlay from './AlbumCardOverlay.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	type ItemType = 'album' | 'artist';
	type TimeRangeCardVariant = 'featured' | 'overview' | 'expanded';

	interface Props {
		item: AlbumRef | ArtistRef;
		itemType: ItemType;
		href?: string | null;
		rank: number;
		variant: TimeRangeCardVariant;
		className: string;
		onFallbackClick: (item: AlbumRef | ArtistRef) => void;
	}

	let { item, itemType, href = null, rank, variant, className, onFallbackClick }: Props = $props();

	const itemName = $derived('title' in item ? item.title : item.name);

	function isAlbum(_value: AlbumRef | ArtistRef): _value is AlbumRef {
		return itemType === 'album';
	}

	function handleCardClick() {
		onFallbackClick(item);
	}

	function handleCardKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			onFallbackClick(item);
		}
	}

	function handleSearchClick(event: Event) {
		event.stopPropagation();
		onFallbackClick(item);
	}
</script>

<Card
	variant="raised"
	padding="none"
	href={href ?? undefined}
	class="group {className} {href
		? ''
		: 'cursor-pointer transition hover:shadow-md motion-safe:hover:-translate-y-0.5 focus-visible:outline-2 focus-visible:outline-primary'}"
	onclick={href ? undefined : handleCardClick}
	onkeydown={href ? undefined : handleCardKeydown}
	role={href ? undefined : 'button'}
	tabindex={href ? undefined : 0}
>
	{#if variant === 'featured'}
		<figure class="relative aspect-square w-full">
			{#if itemType === 'album'}
				<AlbumImage
					mbid={item.musicbrainz_id || ''}
					alt={itemName}
					size="xl"
					requestSize={250}
					rounded="none"
					className="w-full h-full"
					customUrl={(item as AlbumRef).cover_url || null}
				/>
			{:else}
				<ArtistImage
					mbid={item.musicbrainz_id || ''}
					alt={itemName}
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
			{#if isAlbum(item) && item.musicbrainz_id && item.in_library}
				<AlbumCardOverlay
					mbid={item.musicbrainz_id}
					albumName={itemName}
					artistName={(item as AlbumRef).artist_name || 'Unknown'}
					coverUrl={(item as AlbumRef).cover_url || null}
				/>
			{/if}
			<div class="absolute inset-x-0 bottom-0 p-4 text-white">
				<h3 class="line-clamp-2 text-lg font-bold sm:text-xl">{itemName}</h3>
				{#if isAlbum(item) && item.artist_name}
					<p class="line-clamp-1 text-sm text-white/80">{item.artist_name}</p>
				{/if}
				{#if item.listen_count !== null && item.listen_count !== undefined}
					<p class="mt-1 flex items-center gap-1 text-sm text-white/60">
						<Headphones class="h-3.5 w-3.5" aria-hidden="true" />
						{formatListenCount(item.listen_count)}
					</p>
				{/if}
			</div>
			{#if !item.musicbrainz_id}
				<IconButton
					variant="neutral"
					size="xs"
					icon={Search}
					label={itemType === 'album' ? 'Search album' : 'Search artist'}
					class="absolute bottom-3 right-3"
					onclick={handleSearchClick}
				/>
			{/if}
		</figure>
	{:else}
		{#if itemType === 'album'}
			<figure class="relative aspect-square overflow-hidden">
				<AlbumImage
					mbid={item.musicbrainz_id || ''}
					alt={itemName}
					size="md"
					rounded="none"
					className="w-full h-full"
					customUrl={(item as AlbumRef).cover_url || null}
				/>
				{#if item.in_library}
					<div class="badge badge-success badge-sm absolute left-1 top-1 z-20">
						<Check class="h-3 w-3" />
					</div>
				{/if}
				{#if item.musicbrainz_id && item.in_library}
					<AlbumCardOverlay
						mbid={item.musicbrainz_id}
						albumName={itemName}
						artistName={(item as AlbumRef).artist_name || 'Unknown'}
						coverUrl={(item as AlbumRef).cover_url || null}
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
				{#if !item.musicbrainz_id}
					<IconButton
						size="xs"
						icon={Search}
						label="Search album"
						class="absolute bottom-1 right-1"
						onclick={handleSearchClick}
					/>
				{/if}
			</figure>
		{:else}
			<figure
				class={variant === 'overview'
					? 'relative flex justify-center pt-4'
					: 'relative aspect-square overflow-hidden'}
			>
				<ArtistImage
					mbid={item.musicbrainz_id || ''}
					alt={itemName}
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
				{#if !item.musicbrainz_id}
					<IconButton
						size="xs"
						icon={Search}
						label="Search artist"
						class="absolute bottom-1 right-1"
						onclick={handleSearchClick}
					/>
				{/if}
			</figure>
		{/if}
		<div
			class={variant === 'expanded'
				? 'flex flex-col gap-2 px-2 pb-2 sm:px-3 sm:pb-3'
				: 'flex flex-col gap-2 px-2 pb-2'}
		>
			<CardTitle
				as="h3"
				class={variant === 'expanded' ? 'line-clamp-1 text-xs sm:text-sm' : 'line-clamp-1 text-xs'}
			>
				{itemName}
			</CardTitle>
			{#if isAlbum(item) && item.artist_name}
				<p class="line-clamp-1 text-xs text-base-content/50">{item.artist_name}</p>
			{/if}
			{#if item.listen_count !== null && item.listen_count !== undefined}
				<p class="text-xs text-base-content/40">{formatListenCount(item.listen_count)}</p>
			{/if}
		</div>
	{/if}
</Card>
