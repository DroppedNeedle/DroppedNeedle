<script lang="ts">
	import type {
		AlbumRef,
		ChartSection,
		GenreRef,
		SectionItem,
		TrackRef
	} from '$lib/features/discover/queries/types';
	import type { Snippet } from 'svelte';
	import { X, Check, Music2, Tv, Sparkles, Search } from 'lucide-svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
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
	import { authStore } from '$lib/stores/authStore.svelte';
	import { EAGER_IMAGE_COUNT } from '$lib/media/lazyImage';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import AlbumCardOverlay from '$lib/features/album/components/AlbumCardOverlay.svelte';
	import AlbumRequestButton from '$lib/features/requests/components/AlbumRequestButton.svelte';
	import HorizontalCarousel from '$lib/ui/HorizontalCarousel.svelte';
	import SourceBadge from './SourceBadge.svelte';
	import SampleButton from './SampleButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';

	interface Props {
		section: ChartSection;
		showConnectCard?: boolean;
		headerLink?: string | null;
		headerActions?: Snippet;
		hideHeader?: boolean;
		showPreview?: boolean;
		/** The row is in the first screen: its first images load without waiting. */
		eager?: boolean;
		/** Smaller tiles, for a shelf that shares its row with another. */
		compact?: boolean;
	}

	let {
		section,
		showConnectCard = true,
		headerLink = null,
		headerActions,
		hideHeader = false,
		showPreview = true,
		eager = false,
		compact = false
	}: Props = $props();

	const tileWidth = $derived(compact ? 'w-28 sm:w-32 md:w-36' : 'w-32 sm:w-36 md:w-44');

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

	// Where the person connects the service a shelf waits on: their own
	// listening accounts on the profile, a media server in the admin's
	// settings. Null when they cannot connect it themselves.
	const connect = $derived.by(() => {
		switch (section.connect_service) {
			case 'listenbrainz':
				return { name: 'ListenBrainz', href: '/profile#scrobbling' };
			case 'lastfm':
				return { name: 'Last.fm', href: '/profile#scrobbling' };
			case 'jellyfin':
				return authStore.isAdmin ? { name: 'Jellyfin', href: '/settings?tab=jellyfin' } : null;
			default:
				return null;
		}
	});

	function rowKey(item: SectionItem, i: number): string {
		return `${item.kind === 'album' || item.kind === 'track' ? item.title : item.name}-${i}`;
	}
</script>

<section class="mb-6 sm:mb-8">
	{#if !hideHeader}
		<SectionHead
			title={section.title}
			href={headerLink ? withBasePath(headerLink) : null}
			actions={headerActions}
		>
			{#snippet chips()}
				<SourceBadge source={section.source ?? undefined} />
			{/snippet}
		</SectionHead>
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
				<p class="text-muted text-sm">{section.fallback_message}</p>
				{#if connect}
					<Button variant="primary" size="sm" href={withBasePath(connect.href)} class="mt-2">
						Connect {connect.name}
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
					<MediaTile
						kind="artist"
						title={item.name}
						href={artistHref ?? undefined}
						class="{tileWidth} shrink-0 {artistHref ? '' : 'opacity-80'}"
					>
						{#snippet art()}
							<ArtistImage
								mbid={item.musicbrainz_id ?? ''}
								localId={item.local_id}
								alt={item.name}
								size="full"
								className="w-full h-full"
								lazy={lazyAt(i)}
							/>
						{/snippet}
						{#snippet meta()}
							{#if item.listen_count}{formatListenCount(item.listen_count)}{:else}&nbsp;{/if}
						{/snippet}
						{#snippet overlay()}
							{#if !item.musicbrainz_id}
								<StatusChip size="xs" icon={X} class="absolute top-2 left-2">
									<span class="sr-only">Not linked to MusicBrainz</span>
								</StatusChip>
							{/if}
							{#if item.in_library}
								<StatusChip tone="action" size="xs" icon={Check} class="absolute top-2 right-2">
									<span class="sr-only">In library</span>
								</StatusChip>
							{/if}
						{/snippet}
					</MediaTile>
				{:else if item.kind === 'album'}
					{@const albumHref =
						albumHrefOrNull(item.musicbrainz_id) ??
						(item.local_id ? localAlbumHref(item.local_id) : null)}
					{@const isItemRequested = item.requested || libraryStore.isRequested(item.musicbrainz_id)}
					<div class="{tileWidth} shrink-0">
						<MediaTile
							title={item.title}
							href={albumHref ?? undefined}
							class="w-full {albumHref ? '' : 'opacity-90'}"
						>
							{#snippet art()}
								<AlbumImage
									mbid={item.musicbrainz_id || ''}
									alt={item.title}
									size="full"
									className="w-full h-full"
									lazy={lazyAt(i)}
									customUrl={item.cover_url || null}
								/>
							{/snippet}
							{#snippet meta()}
								{item.artist_name || ' '}
							{/snippet}
							{#snippet overlay()}
								{#if item.in_library}
									<StatusChip tone="action" size="xs" icon={Check} class="absolute top-2 left-2">
										<span class="sr-only">In library</span>
									</StatusChip>
								{/if}
								{#if item.musicbrainz_id && item.in_library}
									<AlbumCardOverlay
										mbid={item.musicbrainz_id}
										albumName={item.title}
										artistName={item.artist_name || 'Unknown'}
										coverUrl={item.cover_url || null}
										size="sm"
										playShown
									/>
								{/if}
								{#if !item.musicbrainz_id && !item.local_id}
									<IconButton
										size="xs"
										variant="neutral"
										icon={Search}
										label="Search album"
										class="absolute right-2 bottom-2"
										onclick={(e) => {
											e.stopPropagation();
											handleAlbumSearch(item);
										}}
									/>
								{/if}
							{/snippet}
						</MediaTile>
						{#if item.musicbrainz_id && ((!item.in_library && !isItemRequested) || showPreview)}
							<div class="flex flex-col gap-1.5 px-1 pt-1 pb-1">
								{#if !item.in_library && !isItemRequested}
									<AlbumRequestButton
										mbid={item.musicbrainz_id}
										artistName={item.artist_name ?? ''}
										albumName={item.title}
										artistMbid={item.artist_id ?? undefined}
										labelled
									/>
								{/if}
								{#if showPreview && item.musicbrainz_id}
									<SampleButton
										sampleKey={item.musicbrainz_id}
										artist={item.artist_name ?? ''}
										title={item.title}
										kind="album"
										artistMbid={item.artist_id}
										coverUrl={item.cover_url}
										labelled
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
										<p class="text-xs text-muted line-clamp-1">{item.artist_name}</p>
									{/if}
									{#if item.listened_at}
										<p class="text-xs text-muted">{formatListenedAt(item.listened_at)}</p>
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
