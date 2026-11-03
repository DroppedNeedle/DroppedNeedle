<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';

	interface Props {
		imageId: string;
		imageUrl?: string | null;
		name: string;
		artistName: string;
		onclick: () => void;
	}

	let { imageId, imageUrl, name, artistName, onclick }: Props = $props();
</script>

<button
	class="shrink-0 w-36 group cursor-pointer transition-transform hover:scale-105 active:scale-95 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 rounded-lg"
	{onclick}
	aria-label="View album: {name} by {artistName}"
>
	<div class="aspect-square rounded-lg overflow-hidden shadow-sm">
		<AlbumImage
			mbid={imageId}
			customUrl={imageUrl}
			alt={name}
			size="full"
			requestSize={250}
			rounded="none"
			className="w-full h-full"
		/>
	</div>
	<p class="text-sm font-medium mt-1 line-clamp-1">{name}</p>
	<p class="text-xs opacity-60 line-clamp-1">{artistName}</p>
</button>
