<script lang="ts">
	import { Check } from 'lucide-svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import ArtistCardDownloadButton from '$lib/features/artist/components/ArtistCardDownloadButton.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import type { ArtistRef } from '$lib/features/discover/queries/types';

	interface Props {
		artist: ArtistRef;
		showLibraryBadge?: boolean;
		onclick?: () => void;
		href?: string | null;
	}

	let { artist, showLibraryBadge = false, onclick, href = null }: Props = $props();
</script>

<MediaTile
	kind="artist"
	wrapName
	title={artist.name}
	href={href ?? undefined}
	onclick={href ? undefined : onclick}
	class="w-full"
>
	{#snippet art()}
		<ArtistImage
			mbid={artist.musicbrainz_id || ''}
			localId={artist.local_id}
			alt={artist.name}
			size="full"
			className="w-full h-full"
			lazy={true}
		/>
	{/snippet}
	{#snippet meta()}
		{#if artist.listen_count}
			<span class="uppercase tracking-[0.12em]"
				>{artist.listen_count} album{artist.listen_count !== 1 ? 's' : ''}</span
			>
		{/if}
	{/snippet}
	{#snippet overlay()}
		{#if artist.musicbrainz_id}
			<ArtistCardDownloadButton artistName={artist.name} artistMbid={artist.musicbrainz_id} />
		{/if}
		{#if showLibraryBadge || artist.in_library}
			<StatusChip tone="action" size="xs" icon={Check} class="absolute top-2 left-2">
				<span class="sr-only">In library</span>
			</StatusChip>
		{/if}
	{/snippet}
</MediaTile>
