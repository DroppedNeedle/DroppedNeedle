<script lang="ts">
	import { Check } from 'lucide-svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import ArtistCardDownloadButton from '$lib/components/ArtistCardDownloadButton.svelte';
	import Card from '$lib/ui/Card.svelte';
	import type { ArtistRef } from '$lib/queries/discover/types';

	interface Props {
		artist: ArtistRef;
		showLibraryBadge?: boolean;
		onclick?: () => void;
		href?: string | null;
	}

	let { artist, showLibraryBadge = false, onclick, href = null }: Props = $props();
</script>

<Card
	padding="none"
	href={href ?? undefined}
	onclick={href ? undefined : onclick}
	onkeydown={href || !onclick
		? undefined
		: (e: KeyboardEvent) => {
				if (e.key === 'Enter' || e.key === ' ') {
					e.preventDefault();
					onclick();
				}
			}}
	role={href || !onclick ? undefined : 'button'}
	tabindex={href || !onclick ? undefined : 0}
	class="group {href
		? ''
		: onclick
			? 'cursor-pointer transition-colors hover:bg-base-300 focus-visible:outline-2 focus-visible:outline-primary'
			: 'cursor-default'}"
>
	{#if artist.musicbrainz_id}
		<ArtistCardDownloadButton artistName={artist.name} artistMbid={artist.musicbrainz_id} />
	{/if}
	<figure class="flex justify-center pt-4 relative">
		<ArtistImage
			mbid={artist.musicbrainz_id || ''}
			localId={artist.local_id}
			alt={artist.name}
			size="md"
			lazy={true}
		/>
		{#if showLibraryBadge || artist.in_library}
			<div class="absolute top-2 right-2 badge badge-success badge-sm gap-1 opacity-90">
				<Check class="w-3 h-3" />
			</div>
		{/if}
	</figure>
	<div class="flex flex-col items-center gap-2 px-3 pt-1 pb-3 text-center">
		<h3 class="font-semibold text-sm line-clamp-1">{artist.name}</h3>
		{#if artist.listen_count}
			<p class="text-xs text-base-content/50">
				{artist.listen_count} album{artist.listen_count !== 1 ? 's' : ''}
			</p>
		{/if}
	</div>
</Card>
