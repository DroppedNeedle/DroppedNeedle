<script lang="ts">
	import type { RemotePlaylist } from '$lib/features/remote-library/queries/types';
	import { formatTotalDurationSec } from '$lib/utils/formatting';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		playlist: RemotePlaylist;
		href: string;
	}

	let { playlist, href }: Props = $props();

	let durationText = $derived(
		playlist.duration_secs ? formatTotalDurationSec(playlist.duration_secs) : ''
	);

	let subtitle = $derived(
		`${playlist.track_count} track${playlist.track_count === 1 ? '' : 's'}${durationText ? ` - ${durationText}` : ''}`
	);
</script>

<Card
	variant="raised"
	padding="none"
	{href}
	class="group w-full shrink-0 overflow-hidden"
	aria-label="Open {playlist.name}"
>
	<figure class="aspect-square overflow-hidden">
		<div
			class="h-full w-full transform-gpu transition-transform duration-200 motion-safe:group-hover:scale-105"
		>
			<AlbumImage
				customUrl={playlist.image_url}
				alt={playlist.name}
				size="full"
				requestSize={250}
				rounded="none"
				className="h-full w-full"
			/>
		</div>
	</figure>
	<div class="p-3">
		<h3 class="text-sm font-semibold line-clamp-1">{playlist.name}</h3>
		<p class="text-xs text-muted mt-0.5">{subtitle}</p>
		{#if playlist.is_smart}
			<span class="badge badge-xs badge-info mt-1">Smart playlist</span>
		{/if}
		{#if playlist.is_imported}
			<span class="badge badge-xs badge-success mt-1">Imported</span>
		{/if}
	</div>
</Card>
