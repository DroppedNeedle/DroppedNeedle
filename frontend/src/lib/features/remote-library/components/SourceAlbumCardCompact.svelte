<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';

	interface Props {
		imageId: string;
		imageUrl?: string | null;
		name: string;
		artistName: string;
		onclick: () => void;
	}

	let { imageId, imageUrl, name, artistName, onclick }: Props = $props();
</script>

<MediaTile title={name} {onclick} label="View album: {name} by {artistName}" class="w-36 shrink-0">
	{#snippet art()}
		<AlbumImage
			mbid={imageId}
			customUrl={imageUrl}
			alt={name}
			size="full"
			requestSize={250}
			className="w-full h-full"
		/>
	{/snippet}
	{#snippet meta()}{artistName}{/snippet}
</MediaTile>
