<script lang="ts">
	import type { Artist, EnrichmentSource } from '$lib/types';
	import { artistHref } from '$lib/utils/entityRoutes';
	import { imageMbid, isLocalOnlyRow } from '$lib/queries/search/SearchV3Adapters';
	import { formatListenCount } from '$lib/utils/formatting';
	import { getListenTitle } from '$lib/utils/enrichment';
	import { Music2 } from 'lucide-svelte';
	import ArtistImage from './ArtistImage.svelte';
	import ArtistCardDownloadButton from './ArtistCardDownloadButton.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		artist: Artist;
		enrichmentSource?: EnrichmentSource;
		onenrichmentrequest?: (() => void) | undefined;
	}

	let { artist, enrichmentSource = null, onenrichmentrequest = undefined }: Props = $props();

	let listenTitle = $derived(getListenTitle(enrichmentSource, 'artist'));
</script>

<Card
	variant="raised"
	padding="none"
	href={artistHref(artist.musicbrainz_id)}
	onpointerenter={artist.local_id === artist.musicbrainz_id ? undefined : onenrichmentrequest}
	onfocus={artist.local_id === artist.musicbrainz_id ? undefined : onenrichmentrequest}
	class="group w-full shrink-0"
>
	{#if !artist.local_id || artist.local_id !== artist.musicbrainz_id}
		<ArtistCardDownloadButton artistName={artist.title} artistMbid={artist.musicbrainz_id} />
	{/if}
	<figure class="flex justify-center pt-4">
		<ArtistImage
			mbid={imageMbid(artist)}
			alt={artist.title}
			size="lg"
			requestSize={250}
			responsiveSizes="(max-width: 639px) 144px, 176px"
			remoteUrl={artist.thumb_url ?? null}
			source={artist.local_id ? 'local' : 'provider'}
			available={!isLocalOnlyRow(artist)}
			localId={isLocalOnlyRow(artist) ? artist.local_id : null}
		/>
	</figure>

	<div class="flex flex-col items-center gap-0.5 px-3 pb-3 text-center">
		<h2 class="font-semibold text-sm line-clamp-1">{artist.title}</h2>

		<p class="text-xs text-base-content/60 line-clamp-1 min-h-[1rem]">
			{#if artist.disambiguation}{artist.disambiguation}{:else}&nbsp;{/if}
		</p>

		<div class="flex flex-wrap items-center justify-center gap-1 mt-1 min-h-[1.5rem]">
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
	</div>
</Card>
