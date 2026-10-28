<script lang="ts">
	import type {
		AlbumRef,
		ChartSection,
		GenreRef,
		SectionItem,
		TrackRef
	} from '$lib/queries/discover/types';
	import type { Snippet } from 'svelte';
	import { ArrowRight, X, Check, Music2, Tv, Sparkles, Search } from 'lucide-svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		albumHrefOrNull,
		artistHrefOrNull,
		localAlbumHref,
		localArtistHref
	} from '$lib/utils/entityRoutes';
	import { formatListenCount, formatListenedAt } from '$lib/utils/formatting';
	import { libraryStore } from '$lib/stores/library';
	import { EAGER_IMAGE_COUNT } from '$lib/utils/lazyImage';
	import ArtistImage from './ArtistImage.svelte';
	import AlbumImage from './AlbumImage.svelte';
	import AlbumCardOverlay from './AlbumCardOverlay.svelte';
	import AlbumRequestButton from './AlbumRequestButton.svelte';
	import HorizontalCarousel from './HorizontalCarousel.svelte';
	import SourceBadge from './SourceBadge.svelte';
	import SampleButton from './discover/SampleButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		section: ChartSection;
		showConnectCard?: boolean;
		headerLink?: string | null;
		headerActions?: Snippet;
		hideHeader?: boolean;
		showPreview?: boolean;
		/** The row is in the first screen: its first images load without waiting. */
		eager?: boolean;
	}

	let {
		section,
		showConnectCard = true,
		headerLink = null,
		headerActions,
		hideHeader = false,
		showPreview = true,
		eager = false
	}: Props = $props();

	const lazyAt = (i: number) => !eager || i >= EAGER_IMAGE_COUNT;

	function getGenreHref(genre: GenreRef): string {
		return withBasePath(`/genre?name=${encodeURIComponent(genre.name)}`);
	}

	function handleAlbumSearch(album: AlbumRef) {
		const query = [album.artist_name, album.title].filter(Boolean).join(' ').trim();
		if (query) {
			goto(withBasePath(`/search/albums?q=${encodeURIComponent(query)}`));
		}
	}

	function handleTrackAlbumSearch(track: TrackRef) {
		const query = [track.artist_name, track.album_name || track.title]
			.filter(Boolean)
			.join(' ')
			.trim();
		if (query) {
			goto(withBasePath(`/search/albums?q=${encodeURIComponent(query)}`));
		}
	}

	function rowKey(item: SectionItem, i: number): string {
		return `${item.kind === 'album' || item.kind === 'track' ? item.title : item.name}-${i}`;
	}
</script>

<section class="mb-6 sm:mb-8">
	{#if !hideHeader}
		<div class="flex items-center justify-between mb-3 sm:mb-4">
			<div class="flex items-center gap-2">
				{#if headerLink}
					<a
						href={withBasePath(headerLink)}
						class="section-title text-lg sm:text-xl font-bold hover:text-primary transition-colors"
					>
						{section.title}
					</a>
				{:else}
					<h2 class="section-title text-lg sm:text-xl font-bold">{section.title}</h2>
				{/if}
				<SourceBadge source={section.source ?? undefined} />
			</div>
			{#if headerActions}
				{@render headerActions()}
			{/if}
			{#if headerLink}
				<a
					href={withBasePath(headerLink)}
					class="text-sm text-base-content/50 hover:text-primary transition-colors flex items-center gap-1"
				>
					See all
					<ArrowRight class="w-3.5 h-3.5" />
				</a>
			{/if}
		</div>
	{/if}

	{#if section.items.length === 0 && section.fallback_message && showConnectCard}
		<Card variant="bordered" class="border-dashed">
			<div class="flex flex-col items-center gap-2 text-center">
				<div class="mb-2">
					{#if section.connect_service === 'listenbrainz'}
						<Music2 class="h-5 w-5" />
					{:else if section.connect_service === 'jellyfin'}
						<Tv class="h-5 w-5" />
					{:else if section.connect_service === 'lastfm'}
						<Music2 class="h-5 w-5" />
					{:else}
						<Sparkles class="h-5 w-5" />
					{/if}
				</div>
				<p class="text-base-content/70 text-sm">{section.fallback_message}</p>
				{#if section.connect_service}
					<Button variant="primary" size="sm" href={withBasePath('/settings')} class="mt-2">
						Connect {section.connect_service === 'listenbrainz'
							? 'ListenBrainz'
							: section.connect_service === 'lastfm'
								? 'Last.fm'
								: section.connect_service === 'jellyfin'
									? 'Jellyfin'
									: section.connect_service}
					</Button>
				{/if}
			</div>
		</Card>
	{:else if section.type === 'genres'}
		<div class="flex flex-wrap gap-2">
			{#each section.items as item, i (rowKey(item, i))}
				{#if item.kind === 'genre'}
					<Button variant="outline" size="sm" href={getGenreHref(item)}>
						{item.name}
						{#if item.listen_count}
							<span class="badge badge-ghost badge-xs ml-1"
								>{formatListenCount(item.listen_count)}</span
							>
						{/if}
					</Button>
				{/if}
			{/each}
		</div>
	{:else}
		<HorizontalCarousel class="-mx-4 px-4 sm:mx-0 sm:px-0 pb-2">
			{#each section.items as item, i (rowKey(item, i))}
				{#if item.kind === 'artist'}
					{@const artistHref =
						artistHrefOrNull(item.musicbrainz_id) ??
						(item.local_id ? localArtistHref(item.local_id) : null)}
					<div class="w-32 sm:w-36 md:w-44 shrink-0">
						<Card
							variant="raised"
							padding="none"
							href={artistHref ?? undefined}
							data-sveltekit-preload-data={artistHref ? 'hover' : undefined}
							class="w-full {artistHref ? '' : 'cursor-default opacity-80'}"
						>
							<figure class="flex justify-center pt-4 relative">
								<ArtistImage
									mbid={item.musicbrainz_id ?? ''}
									localId={item.local_id}
									alt={item.name}
									size="md"
									lazy={lazyAt(i)}
								/>
								{#if !item.musicbrainz_id}
									<div
										class="absolute top-2 left-2 badge badge-ghost badge-sm"
										title="Not linked to MusicBrainz"
									>
										<X class="w-3 h-3" />
									</div>
								{/if}
								{#if item.in_library}
									<div class="absolute top-2 right-2 badge badge-success badge-sm">
										<Check class="w-3.5 h-3.5" />
									</div>
								{/if}
							</figure>
							<div class="flex flex-col items-center gap-2 px-2 pb-2 text-center">
								<CardTitle as="h3" class="text-xs line-clamp-1">{item.name}</CardTitle>
								{#if item.listen_count}
									<p class="text-xs text-base-content/50">{formatListenCount(item.listen_count)}</p>
								{/if}
							</div>
						</Card>
					</div>
				{:else if item.kind === 'album'}
					{@const albumHref =
						albumHrefOrNull(item.musicbrainz_id) ??
						(item.local_id ? localAlbumHref(item.local_id) : null)}
					{@const isItemRequested = item.requested || libraryStore.isRequested(item.musicbrainz_id)}
					<div class="w-32 sm:w-36 md:w-44 shrink-0">
						<Card
							variant="raised"
							padding="none"
							href={albumHref ?? undefined}
							class="group w-full {albumHref ? '' : 'cursor-default opacity-90'}"
						>
							<figure class="aspect-square overflow-hidden relative">
								<AlbumImage
									mbid={item.musicbrainz_id || ''}
									alt={item.title}
									size="md"
									rounded="none"
									className="w-full h-full"
									lazy={lazyAt(i)}
									customUrl={item.cover_url || null}
								/>
								{#if item.in_library}
									<div class="absolute top-2 left-2 z-20 badge badge-success badge-sm">
										<Check class="w-3.5 h-3.5" />
									</div>
								{/if}
								{#if item.musicbrainz_id && item.in_library}
									<AlbumCardOverlay
										mbid={item.musicbrainz_id}
										albumName={item.title}
										artistName={item.artist_name || 'Unknown'}
										coverUrl={item.cover_url || null}
										size="sm"
									/>
								{/if}
								{#if !item.musicbrainz_id && !item.local_id}
									<IconButton
										size="xs"
										icon={Search}
										label="Search album"
										class="absolute bottom-2 right-2"
										onclick={(e) => {
											e.stopPropagation();
											handleAlbumSearch(item);
										}}
									/>
								{/if}
							</figure>
							<div class="flex flex-col gap-2 px-2 pb-2">
								<CardTitle as="h3" class="text-xs line-clamp-1">{item.title}</CardTitle>
								{#if item.artist_name}
									<p class="text-xs text-base-content/50 line-clamp-1">{item.artist_name}</p>
								{/if}
							</div>
						</Card>
						{#if item.musicbrainz_id && ((!item.in_library && !isItemRequested) || showPreview)}
							<div class="flex items-center justify-center gap-1 mt-1 pb-1">
								{#if !item.in_library && !isItemRequested}
									<AlbumRequestButton
										mbid={item.musicbrainz_id}
										artistName={item.artist_name ?? ''}
										albumName={item.title}
										artistMbid={item.artist_id ?? undefined}
									/>
								{/if}
								{#if showPreview && item.musicbrainz_id}
									<SampleButton
										sampleKey={item.musicbrainz_id}
										artist={item.artist_name ?? ''}
										title={item.title}
										kind="album"
										size="sm"
										artistMbid={item.artist_id}
										coverUrl={item.cover_url}
									/>
								{/if}
							</div>
						{/if}
					</div>
				{:else if item.kind === 'track'}
					{@const trackArtistHref = artistHrefOrNull(item.artist_id)}
					<div class="w-56 sm:w-64 md:w-72 shrink-0">
						<Card
							variant="raised"
							padding="none"
							href={trackArtistHref ?? undefined}
							class="w-full {trackArtistHref ? '' : 'cursor-default opacity-90'}"
						>
							<div class="flex items-center">
								<figure class="w-16 h-16 shrink-0 rounded-l-box">
									{#if item.cover_url}
										<AlbumImage
											customUrl={item.cover_url}
											alt={item.album_name || item.title}
											size="full"
											rounded="none"
											className="w-full h-full"
											lazy={lazyAt(i)}
										/>
									{:else}
										<div class="relative w-full h-full overflow-hidden">
											<CoverPlaceholder name={item.album_name || item.title} />
										</div>
									{/if}
								</figure>
								<div class="flex min-w-0 flex-1 flex-col justify-center gap-2 p-2">
									<CardTitle as="h3" class="text-xs line-clamp-1">{item.title}</CardTitle>
									{#if item.artist_name}
										<p class="text-xs text-base-content/50 line-clamp-1">{item.artist_name}</p>
									{/if}
									{#if item.listened_at}
										<p class="text-xs text-base-content/40">{formatListenedAt(item.listened_at)}</p>
									{/if}
									{#if !item.artist_id}
										<div class="flex justify-end pt-1">
											<IconButton
												size="xs"
												icon={Search}
												label="Search album"
												onclick={(e) => {
													e.stopPropagation();
													handleTrackAlbumSearch(item);
												}}
											/>
										</div>
									{/if}
								</div>
							</div>
						</Card>
					</div>
				{/if}
			{/each}
		</HorizontalCarousel>
	{/if}
</section>
