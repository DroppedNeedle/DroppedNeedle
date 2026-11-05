<!--
	One artist result: the Library artist tile with the type and description
	under the name, then the release count and the listen count once they are
	fetched, and the discography download over the picture.
-->
<script lang="ts">
	import type { Artist } from '$lib/features/search/queries/types';
	import type { EnrichmentSource } from '$lib/features/album/queries/types';
	import { artistHref } from '$lib/utils/entityRoutes';
	import { imageMbid, isLocalOnlyRow } from '$lib/features/search/queries/SearchV3Adapters';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import ArtistCardDownloadButton from '$lib/features/artist/components/ArtistCardDownloadButton.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import SearchListens from './SearchListens.svelte';

	interface Props {
		artist: Artist;
		enrichmentSource?: EnrichmentSource;
		onenrichmentrequest?: (() => void) | undefined;
	}

	let { artist, enrichmentSource = null, onenrichmentrequest = undefined }: Props = $props();

	const artistLine = $derived(
		[artist.type_info, artist.disambiguation].filter(Boolean).join(' · ')
	);
</script>

<MediaTile
	kind="artist"
	title={artist.title}
	titleAs="h3"
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
		{#if artistLine}{artistLine}{:else}&nbsp;{/if}
	{/snippet}
	<!-- Both lines are kept while empty, so enrichment fills them in place. -->
	{#snippet footer()}
		<div class="flex flex-col items-center gap-1.5">
			<span class="flex h-5 items-center">
				{#if artist.release_group_count != null}
					<span
						class="rounded-sm border border-edge px-1.5 font-mono text-[0.6875rem] leading-[1.125rem] tracking-wider text-base-content uppercase"
					>
						{artist.release_group_count}
						{artist.release_group_count === 1 ? 'release' : 'releases'}
					</span>
				{/if}
			</span>
			<span class="flex h-4 items-center">
				{#if artist.listen_count != null}
					<SearchListens count={artist.listen_count} source={enrichmentSource} kind="artist" />
				{/if}
			</span>
		</div>
	{/snippet}
	{#snippet overlay()}
		{#if !artist.local_id || artist.local_id !== artist.musicbrainz_id}
			<ArtistCardDownloadButton artistName={artist.title} artistMbid={artist.musicbrainz_id} />
		{/if}
	{/snippet}
</MediaTile>
