<script lang="ts">
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { ChevronRight } from 'lucide-svelte';
	import { reveal } from '$lib/actions/reveal';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import type { RemoteAlbum } from '$lib/queries/remotes/types';

	interface Props {
		albums: RemoteAlbum[];
		maxItems?: number;
		seeAllHref?: string;
		seeAllLabel?: string;
		onAlbumClick?: (album: RemoteAlbum) => void;
	}

	let {
		albums,
		maxItems = 12,
		seeAllHref,
		seeAllLabel = 'View all',
		onAlbumClick
	}: Props = $props();

	let visible = $derived(albums.slice(0, maxItems));

	let hoveredIdx = $state<number | null>(null);
</script>

<div use:reveal={{ stagger: 60 }}>
	<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
		{#each visible as album, i (album.id)}
			{@const isSibling = hoveredIdx !== null && hoveredIdx !== i}
			<button
				class="group cursor-pointer text-left transition-all focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100"
				style="transition: transform 0.25s var(--ease-overshoot), opacity 0.2s ease, box-shadow 0.3s var(--ease-spring); will-change: transform; transform: {hoveredIdx ===
				i
					? 'scale(1.04)'
					: isSibling
						? 'scale(0.98)'
						: 'scale(1)'}; {hoveredIdx === i ? 'z-index: 1;' : ''} {isSibling
					? 'opacity: 0.8;'
					: ''}"
				aria-label="Open {album.title} by {album.artist_name}"
				onpointerenter={() => (hoveredIdx = i)}
				onpointerleave={() => (hoveredIdx = null)}
				onclick={() => onAlbumClick?.(album)}
			>
				<div
					class="aspect-square overflow-hidden rounded-xl shadow-sm transition-shadow duration-300 {hoveredIdx ===
					i
						? 'shadow-2xl'
						: ''}"
				>
					<AlbumImage
						mbid={album.release_group_mbid ?? album.id}
						customUrl={album.image_url}
						alt={album.title}
						size="full"
						requestSize={250}
						rounded="none"
						className="h-full w-full"
					/>
				</div>
				<p class="mt-1.5 line-clamp-1 text-sm font-medium">{album.title}</p>
				<p class="line-clamp-1 text-xs text-base-content/50">{album.artist_name}</p>
			</button>
		{/each}
	</div>

	{#if seeAllHref && albums.length > maxItems}
		<div class="mt-4 flex justify-center">
			<Button
				variant="ghost"
				size="sm"
				icon={ChevronRight}
				iconPosition="end"
				href={withBasePath(seeAllHref)}
			>
				{seeAllLabel}
			</Button>
		</div>
	{/if}
</div>
