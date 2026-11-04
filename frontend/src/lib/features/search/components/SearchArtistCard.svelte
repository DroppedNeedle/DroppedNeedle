<script lang="ts">
	import type { Artist } from '$lib/features/search/queries/types';
	import type { EnrichmentSource } from '$lib/features/album/queries/types';
	import { artistHref } from '$lib/utils/entityRoutes';
	import { imageMbid, isLocalOnlyRow } from '$lib/features/search/queries/SearchV3Adapters';
	import { formatListenCount } from '$lib/utils/formatting';
	import { getListenTitle } from '$lib/features/search/queries/enrichment';
	import { Music2 } from 'lucide-svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import ArtistCardDownloadButton from '$lib/features/artist/components/ArtistCardDownloadButton.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';

	interface Props {
		artist: Artist;
		enrichmentSource?: EnrichmentSource;
		onenrichmentrequest?: (() => void) | undefined;
	}

	let { artist, enrichmentSource = null, onenrichmentrequest = undefined }: Props = $props();

	let listenTitle = $derived(getListenTitle(enrichmentSource, 'artist'));
</script>

<MediaTile
	kind="artist"
	title={artist.title}
	titleAs="h2"
	href={artistHref(artist.musicbrainz_id)}
	onintent={artist.local_id === artist.musicbrainz_id ? undefined : onenrichmentrequest}
	class="w-full shrink-0"
>
	{#snippet art()}
		<ArtistImage
			mbid={imageMbid(artist)}
			alt={artist.title}
			size="full"
			className="w-full h-full"
			requestSize={250}
			responsiveSizes="(max-width: 639px) 144px, 176px"
			remoteUrl={artist.thumb_url ?? null}
			source={artist.local_id ? 'local' : 'provider'}
			available={!isLocalOnlyRow(artist)}
			localId={isLocalOnlyRow(artist) ? artist.local_id : null}
		/>
	{/snippet}
	{#snippet meta()}
		{#if artist.disambiguation}{artist.disambiguation}{:else}&nbsp;{/if}
	{/snippet}
	{#snippet footer()}
		<div class="flex min-h-[1.5rem] flex-wrap items-center justify-center gap-1">
			{#if artist.release_group_count != null}
				<span class="badge badge-sm badge-ghost">
					{artist.release_group_count} release{artist.release_group_count !== 1 ? 's' : ''}
				</span>
			{/if}
			{#if artist.listen_count != null}
				{#if enrichmentSource === 'lastfm'}
					<span class="badge badge-sm border-0 bg-lastfm/15 text-lastfm" title={listenTitle}>
						Last.fm {formatListenCount(artist.listen_count, true)}
					</span>
				{:else if enrichmentSource === 'listenbrainz'}
					<span
						class="badge badge-sm border-0 bg-listenbrainz/15 text-listenbrainz"
						title={listenTitle}
					>
						LB {formatListenCount(artist.listen_count, true)}
					</span>
				{:else}
					<span class="badge badge-sm badge-ghost" title={listenTitle}>
						<Music2 class="inline h-3 w-3" />
						{formatListenCount(artist.listen_count, true)}
					</span>
				{/if}
			{/if}
		</div>
	{/snippet}
	{#snippet overlay()}
		{#if !artist.local_id || artist.local_id !== artist.musicbrainz_id}
			<ArtistCardDownloadButton artistName={artist.title} artistMbid={artist.musicbrainz_id} />
		{/if}
	{/snippet}
</MediaTile>
