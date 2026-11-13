<script lang="ts">
	import { goto } from '$app/navigation';
	import { ApiError } from '$lib/api/errors';
	import { useDiscoverActivity } from '$lib/features/discover/queries/DiscoverDemand.svelte';
	import ArtistHeaderSkeleton from '$lib/features/artist/components/ArtistHeaderSkeleton.svelte';
	import AlbumGridSkeleton from '$lib/features/artist/components/AlbumGridSkeleton.svelte';
	import ArtistWhereToBuy from '$lib/features/artist/components/ArtistWhereToBuy.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import BackButton from '$lib/ui/BackButton.svelte';
	import ContextMenu, { type MenuItem } from '$lib/ui/ContextMenu.svelte';
	import JumpTabs, { type JumpSection } from '$lib/ui/JumpTabs.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import type { Stat } from '$lib/ui/StatRow.svelte';
	import ArtistHero from '$lib/features/artist/components/ArtistHero.svelte';
	import ArtistAbout, { type AboutFact } from '$lib/features/artist/components/ArtistAbout.svelte';
	import ArtistDiscography from '$lib/features/artist/components/ArtistDiscography.svelte';
	import ArtistDiscoverMore from '$lib/features/artist/components/ArtistDiscoverMore.svelte';
	import ArtistGenres from '$lib/features/artist/components/ArtistGenres.svelte';
	import PlayFromChooser from '$lib/features/artist/components/PlayFromChooser.svelte';
	import SimilarArtistsCarousel from '$lib/features/artist/components/SimilarArtistsCarousel.svelte';
	import TopSongsList from '$lib/features/artist/components/TopSongsList.svelte';
	import TopAlbumsList from '$lib/features/artist/components/TopAlbumsList.svelte';
	import ArtistLibraryAlbums from '$lib/features/library/components/ArtistLibraryAlbums.svelte';
	import ArtistAppearancesSection from '$lib/features/library/components/ArtistAppearancesSection.svelte';
	import ArtistMusicBrainzLink from '$lib/features/library/components/ArtistMusicBrainzLink.svelte';
	import FavoriteHeart from '$lib/features/library/components/FavoriteHeart.svelte';
	import FollowControl from '$lib/features/following/components/FollowControl.svelte';
	import LocalArtistPage from './LocalArtistPage.svelte';
	import type { AlbumView, ArtistView } from '$lib/features/library/queries/types';
	import { createRequestAlbumMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import { libraryStore } from '$lib/stores/library';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { type MusicSource, isMusicSource } from '$lib/stores/musicSource';
	import {
		getArtistLastFmEnrichmentQuery,
		getArtistReleasesInfiniteQuery,
		getArtistTopAlbumsQuery,
		getArtistTopSongsQuery,
		getBasicArtistQuery,
		getExtendedArtistQuery,
		getSimilarArtistsQuery,
		refreshArtist,
		updateArtistReleaseInCache
	} from '$lib/features/artist/queries/ArtistQueries.svelte';
	import { getConnectionsQuery } from '$lib/features/profile/queries/ConnectionsQuery.svelte';
	import { PAGE_SOURCE_KEYS } from '$lib/constants';
	import { PersistedState } from 'runed';
	import SimpleSourceSwitcher from '$lib/features/discover/components/SimpleSourceSwitcher.svelte';
	import {
		discographyDownloadStore,
		type DiscographyRelease
	} from '$lib/features/downloads/state/discographyDownload.svelte';
	import {
		CircleAlert,
		CirclePlus,
		Download,
		ExternalLink,
		FingerprintPattern,
		ListMusic,
		ListPlus,
		Play,
		RefreshCw,
		Shuffle
	} from 'lucide-svelte';
	import { listeningSourceNotice } from '$lib/features/artist/components/listeningSourceNotice';
	import { mergeArtistReleasePages } from '$lib/features/artist/queries/artistReleasePages';
	import { mergeArtistImages } from '$lib/features/album/queries/catalogAdapters';
	import type { ReleaseGroup } from '$lib/features/album/queries/types';
	import {
		createPlaybackSource,
		createReleasePlayback
	} from '$lib/features/artist/state/artistPlayback.svelte';
	import { outsideLink } from '$lib/ui/outsideLink';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		getRatingsQuery,
		ratingOf,
		createSetRatingMutation
	} from '$lib/features/library/queries/ratings.svelte';
	import { createSetFavoriteMutation } from '$lib/features/library/queries/FavoriteMutations.svelte';

	interface Props {
		data: { artistId: string; primarySource: MusicSource };
		localArtist?: ArtistView;
	}

	let { data, localArtist }: Props = $props();

	// Only an artist the library holds is rated, under its local id.
	const ratingsQuery = getRatingsQuery(() =>
		localArtist ? { artistIds: [localArtist.id] } : null
	);
	const rate = createSetRatingMutation();
	const heart = createSetFavoriteMutation();

	// svelte-ignore state_referenced_locally
	let activeSource = new PersistedState<MusicSource>(
		PAGE_SOURCE_KEYS['artist'],
		data.primarySource
	);
	let selectedSource = $derived(
		isMusicSource(activeSource.current) ? activeSource.current : data.primarySource
	);

	const connectionsQuery = getConnectionsQuery();
	const connectionsSettled = $derived(connectionsQuery.isPending !== true);
	// data-defined plus not-error already implies success: pending and
	// loading-error states carry no data, and refetch-error states report
	// isError, so no separate isSuccess clause is needed.
	const connectionsUsable = $derived(
		connectionsSettled && connectionsQuery.data !== undefined && connectionsQuery.isError !== true
	);
	const linkedSources = $derived.by<MusicSource[]>(() => {
		if (!connectionsUsable) return [];

		const services = connectionsQuery.data?.connections ?? [];
		// A Last.fm row without an account name is a saved key pair, not a link.
		return (['listenbrainz', 'lastfm'] as const).filter((source) =>
			services.some(
				(connection) =>
					connection.service === source && (source !== 'lastfm' || Boolean(connection.username))
			)
		);
	});
	const resolvedSource = $derived.by<MusicSource>(() => {
		if (!connectionsUsable || linkedSources.length === 0) return selectedSource;
		if (linkedSources.includes(selectedSource)) return selectedSource;
		if (linkedSources.includes(data.primarySource)) return data.primarySource;
		return linkedSources.includes('listenbrainz') ? 'listenbrainz' : 'lastfm';
	});
	const discoveryEnabled = $derived(connectionsSettled);
	let albumsDemandEl = $state<HTMLElement>();
	let songsDemandEl = $state<HTMLElement>();
	let similarDemandEl = $state<HTMLElement>();
	useDiscoverActivity(
		() =>
			discoveryEnabled
				? {
						feature: 'artist',
						artist_mbid: data.artistId,
						section: 'top_albums',
						provider: resolvedSource
					}
				: null,
		() => albumsDemandEl
	);
	useDiscoverActivity(
		() =>
			discoveryEnabled
				? {
						feature: 'artist',
						artist_mbid: data.artistId,
						section: 'top_songs',
						provider: resolvedSource
					}
				: null,
		() => songsDemandEl
	);
	useDiscoverActivity(
		() =>
			discoveryEnabled
				? {
						feature: 'artist',
						artist_mbid: data.artistId,
						section: 'similar',
						provider: resolvedSource
					}
				: null,
		() => similarDemandEl
	);

	$effect(() => {
		if (!connectionsUsable || linkedSources.length === 0) return;
		if (linkedSources.includes(selectedSource)) return;

		activeSource.current = resolvedSource;
	});

	let requestedReleaseIds = $state(new Set<string>());

	const artistBasicQuery = getBasicArtistQuery(() => data.artistId);
	const artistBasic = $derived(artistBasicQuery.data);
	const loadingBasic = $derived(artistBasicQuery.isLoading);
	const artistBasicApiError = $derived(
		artistBasicQuery.error instanceof ApiError ? artistBasicQuery.error : null
	);
	const artistNotFound = $derived(artistBasicApiError?.status === 404);
	const providerUnavailable = $derived.by(() => {
		const status = artistBasicApiError?.status;
		return status === 0 || status === 429 || (status !== undefined && status >= 500);
	});

	const artistExtendedQuery = getExtendedArtistQuery(() => data.artistId);
	const artistExtended = $derived(artistExtendedQuery.data);
	const loadingExtended = $derived(artistExtendedQuery.isLoading);

	const similarArtistsQuery = getSimilarArtistsQuery(() => ({
		artistId: data.artistId,
		source: resolvedSource,
		enabled: discoveryEnabled
	}));
	const similarArtists = $derived(discoveryEnabled ? similarArtistsQuery.data : undefined);
	const loadingSimilar = $derived(!discoveryEnabled || similarArtistsQuery.isLoading);

	const topSongsQuery = getArtistTopSongsQuery(() => ({
		artistId: data.artistId,
		source: resolvedSource,
		enabled: discoveryEnabled
	}));
	const topSongs = $derived(discoveryEnabled ? topSongsQuery.data : undefined);
	const loadingTopSongs = $derived(!discoveryEnabled || topSongsQuery.isLoading);

	const topAlbumsQuery = getArtistTopAlbumsQuery(() => ({
		artistId: data.artistId,
		source: resolvedSource,
		enabled: discoveryEnabled
	}));
	const topAlbums = $derived(discoveryEnabled ? topAlbumsQuery.data : undefined);
	const sourceNotice = $derived(listeningSourceNotice([topSongs, topAlbums, similarArtists]));
	const loadingTopAlbums = $derived(!discoveryEnabled || topAlbumsQuery.isLoading);
	// Every list answered that no listening service can be read: one card says so.
	const noService = $derived(
		!loadingTopAlbums &&
			!loadingTopSongs &&
			!loadingSimilar &&
			topAlbums?.configured === false &&
			topSongs?.configured === false &&
			similarArtists?.configured === false
	);
	// Unconfigured ListenBrainz means an admin turned its recommendations off.
	const recommendationsOff = $derived(noService && similarArtists?.source === 'listenbrainz');

	const lastFmEnrichmentQuery = getArtistLastFmEnrichmentQuery(() => ({
		artistId: data.artistId,
		artistName: artistBasic?.name
	}));
	const lastfmEnrichment = $derived(lastFmEnrichmentQuery.data);
	const loadingLastfm = $derived(lastFmEnrichmentQuery.isLoading);

	let error: string | null = $derived.by(() => {
		if (artistNotFound) return 'Artist not found.';
		if (artistBasicQuery.error && !providerUnavailable) {
			return 'Failed to load artist information.';
		}
		if (artistExtendedQuery.error) {
			return 'Failed to load extended artist information.';
		}
		return null;
	});
	const artist = $derived.by(() => {
		if (!artistBasic) return null;
		return {
			...artistBasic,
			...mergeArtistImages(artistBasic, artistExtended?.images),
			description: artistExtended?.description,
			image: artistExtended?.image
		};
	});

	const releasesQuery = getArtistReleasesInfiniteQuery(() => data.artistId);
	const loadingMoreReleases = $derived(releasesQuery.isFetchingNextPage);
	const hasMoreReleases = $derived(releasesQuery.hasNextPage);
	const releases = $derived(mergeArtistReleasePages(releasesQuery.data?.pages ?? []));
	const loadedReleaseCount = $derived(
		releases.albums.length + releases.singles.length + releases.eps.length
	);
	const initialReleasesLoading = $derived(releasesQuery.isLoading);
	const sourceTotalCount = $derived(releasesQuery.data?.pages[0]?.source_total_count ?? null);

	const refreshingArtist = $derived(
		artistBasicQuery.isRefetching || artistExtendedQuery.isRefetching
	);

	const providerReleaseRequest = createRequestAlbumMutation();

	async function handleRequest(releaseId: string, releaseTitle?: string) {
		requestedReleaseIds.add(releaseId);
		requestedReleaseIds = requestedReleaseIds;

		try {
			const result = await providerReleaseRequest
				.mutateAsync({
					release_group_mbid: releaseId,
					artist_name: artist?.name,
					album_title: releaseTitle
				})
				.catch(() => null);

			if (result?.success && artist) {
				await updateArtistReleaseInCache(data.artistId, {
					id: releaseId,
					requested: true
				});
			}
		} finally {
			requestedReleaseIds.delete(releaseId);
			requestedReleaseIds = requestedReleaseIds;
		}
	}

	function handleReleaseRemoved() {
		void refreshArtist(data.artistId);
	}

	function handleLoadMoreReleases() {
		if (!hasMoreReleases || loadingMoreReleases) return;
		void releasesQuery.fetchNextPage();
	}

	let allReleases = $derived([...releases.albums, ...releases.eps, ...releases.singles]);
	let downloadableReleaseCount = $derived(
		allReleases.filter(
			(r) =>
				!r.in_library &&
				!libraryStore.isInLibrary(r.id) &&
				!r.requested &&
				!libraryStore.isRequested(r.id)
		).length
	);

	function openDownloadModal(sectionReleases: ReleaseGroup[], type?: string) {
		if (!artist) return;
		const items: DiscographyRelease[] = sectionReleases.map((r) => ({
			id: r.id,
			title: r.title,
			type: type ?? r.type ?? 'Album',
			year: r.year,
			in_library: libraryStore.isInLibrary(r.id) || r.in_library,
			requested: r.requested || libraryStore.isRequested(r.id)
		}));
		discographyDownloadStore.show(artist.name, data.artistId, items);
	}

	// Playing everything the library holds of the artist, from one source.
	const playSource = createPlaybackSource(() => data.artistId);
	const inLibraryReleases = $derived(allReleases.filter((r) => r.in_library));
	const allPlayback = createReleasePlayback(() => ({
		artistName: artist?.name ?? '',
		artistId: data.artistId,
		releases: inLibraryReleases,
		source: playSource.selected
	}));
	const canPlayAll = $derived(inLibraryReleases.length > 0 && playSource.available.length > 0);

	// Playing the one album the "In your library" card features.
	let albumTarget = $state<ReleaseGroup | null>(null);
	const albumPlayback = createReleasePlayback(() => ({
		artistName: artist?.name ?? '',
		artistId: data.artistId,
		releases: albumTarget ? [albumTarget] : [],
		source: playSource.selected
	}));
	function targetAlbum(album: AlbumView, kind: string) {
		albumTarget = {
			id: album.release_group_mbid ?? album.id,
			title: album.title,
			year: album.year,
			type: kind,
			in_library: true,
			requested: false
		};
	}
	function albumMenu(album: AlbumView, kind: string): MenuItem[] {
		return [
			{
				label: 'Shuffle album',
				icon: Shuffle,
				onclick: () => {
					targetAlbum(album, kind);
					albumPlayback.shuffle();
				}
			},
			{
				label: 'Add to Queue',
				icon: ListPlus,
				onclick: () => {
					targetAlbum(album, kind);
					void albumPlayback.addToQueue();
				}
			},
			{
				label: 'Add to Playlist',
				icon: ListMusic,
				onclick: () => {
					targetAlbum(album, kind);
					void albumPlayback.addToPlaylist();
				}
			}
		];
	}

	$effect(() => () => {
		allPlayback.abort();
		albumPlayback.abort();
	});

	const musicbrainzUrl = $derived(
		outsideLink(`https://musicbrainz.org/artist/${encodeURIComponent(data.artistId)}`) ?? null
	);

	const menuItems = $derived.by<MenuItem[]>(() => {
		const items: MenuItem[] = [];
		if (canPlayAll) {
			items.push(
				{
					label: 'Add All to Queue',
					icon: ListPlus,
					onclick: () => void allPlayback.addToQueue()
				},
				{
					label: 'Add All to Playlist',
					icon: CirclePlus,
					onclick: () => void allPlayback.addToPlaylist()
				}
			);
		}
		if (downloadableReleaseCount > 0) {
			items.push({
				label: `Download Discography (${downloadableReleaseCount})`,
				icon: Download,
				onclick: () => openDownloadModal(allReleases)
			});
		}
		if (artist?.in_library) {
			items.push({
				label: 'Refresh artist info',
				icon: RefreshCw,
				disabled: refreshingArtist,
				onclick: () => void refreshArtist(data.artistId)
			});
		}
		if (musicbrainzUrl) {
			const url = musicbrainzUrl;
			items.push({
				label: 'Open in MusicBrainz',
				icon: ExternalLink,
				onclick: () => window.open(url, '_blank', 'noopener,noreferrer')
			});
		}
		if (authStore.isAdmin && localArtist) {
			const desk = withBasePath(
				`/library/management/artists?artist=${encodeURIComponent(localArtist.id)}&q=${encodeURIComponent(localArtist.name)}`
			);
			items.push({
				label: 'Open artist identity desk',
				icon: FingerprintPattern,
				onclick: () => void goto(desk)
			});
		}
		return items;
	});

	function typeLabel(type: string | null | undefined): string | null {
		if (!type) return null;
		return type === 'Group' ? 'Band' : type === 'Person' ? 'Artist' : type;
	}

	const year = (date: string | null | undefined) => date?.slice(0, 4) || null;

	const heroFacts = $derived.by<string[]>(() => {
		if (!artist) return [];
		const begin = year(artist.life_span?.begin);
		const end = year(artist.life_span?.end);
		const span = begin
			? end
				? `${begin} - ${end}`
				: artist.life_span?.ended
					? begin
					: `${begin} - present`
			: null;
		return [typeLabel(artist.type), artist.disambiguation, artist.country, span].filter(
			(fact): fact is string => !!fact
		);
	});

	const heroStats = $derived.by<Stat[]>(() => {
		const total = sourceTotalCount ?? loadedReleaseCount;
		return [
			{ label: 'Releases', value: total },
			{ label: 'Albums', value: releases.albums.length },
			{ label: 'EPs', value: releases.eps.length },
			{ label: 'Singles', value: releases.singles.length }
		].filter((stat) => stat.value > 0);
	});

	const genreTags = $derived([
		...(artist?.tags ?? []).slice(0, 10),
		...(lastfmEnrichment?.tags ?? []).slice(0, 10).map((tag) => tag.name)
	]);

	const aboutFacts = $derived.by<AboutFact[]>(() => {
		if (!artist) return [];
		const person = artist.type === 'Person';
		const facts: AboutFact[] = [];
		if (artist.life_span?.begin)
			facts.push({ label: person ? 'Born' : 'Formed', value: artist.life_span.begin });
		if (artist.life_span?.end)
			facts.push({ label: person ? 'Died' : 'Ended', value: artist.life_span.end });
		if (artist.country) facts.push({ label: 'Origin', value: artist.country });
		if (genreTags.length > 0)
			facts.push({ label: 'Genres', value: genreTags.slice(0, 3).join(', ') });
		const type = typeLabel(artist.type);
		if (type) facts.push({ label: 'Type', value: type });
		return facts;
	});

	const ownsAlbums = $derived(!!localArtist && localArtist.album_count > 0);
	// The genres sit under the library card, else beside the row after About.
	const genresBelow = $derived(!ownsAlbums && genreTags.length > 0);

	const tocSections = $derived.by<JumpSection[]>(() => {
		if (!artist) return [];
		return [
			{ id: 'section-overview', label: 'Overview' },
			...(ownsAlbums ? [{ id: 'section-library-albums', label: 'In your library' }] : []),
			...(artist.appears_in_library
				? [{ id: 'section-library-appearances', label: 'Appears on' }]
				: []),
			{ id: 'section-about', label: 'About' },
			...(noService
				? []
				: [
						{ id: 'section-popular', label: 'Popular' },
						{ id: 'section-similar', label: 'Similar' }
					]),
			...(loadedReleaseCount > 0 ? [{ id: 'section-discography', label: 'Discography' }] : [])
		];
	});
</script>

{#snippet back()}
	<BackButton />
{/snippet}

<div class="w-full">
	{#if artistNotFound}
		<div class="dn-artist-page">
			{@render back()}
			<div class="flex min-h-[50vh] items-center justify-center">
				<div class="alert alert-error">
					<span>Artist not found.</span>
				</div>
			</div>
		</div>
	{:else if providerUnavailable && localArtist}
		<div class="dn-artist-page pb-0">
			<div class="alert alert-info text-sm">
				<span
					>MusicBrainz is unreachable right now, so this page is built from your local files. Some
					extras are hidden until it returns.</span
				>
			</div>
		</div>
		<LocalArtistPage artistId={localArtist.id} />
	{:else if providerUnavailable}
		<div class="dn-artist-page">
			{@render back()}
			<div class="flex min-h-[50vh] items-center justify-center">
				<div class="alert alert-error">
					<span>MusicBrainz is temporarily unavailable.</span>
					<Button variant="ghost" size="sm" onclick={() => void artistBasicQuery.refetch()}>
						Retry
					</Button>
				</div>
			</div>
		</div>
	{:else if error}
		<div class="dn-artist-page">
			{@render back()}
			<div class="flex min-h-[50vh] items-center justify-center">
				<div class="alert alert-error">
					<span>{error}</span>
				</div>
			</div>
		</div>
	{:else if loadingBasic && !artist}
		<ArtistHeaderSkeleton />
		<div class="dn-artist-page">
			<AlbumGridSkeleton title="Discography" count={8} />
		</div>
	{:else if artist}
		<ArtistHero
			name={artist.name}
			mbid={artist.musicbrainz_id}
			remoteUrl={artist.thumb_url}
			facts={heroFacts}
			stats={heroStats}
			{back}
		>
			{#snippet badges()}
				{#if artist.in_library}
					<StatusChip tone="action">In library</StatusChip>
				{:else if artist.appears_in_library}
					<StatusChip tone="info">Appears in library</StatusChip>
				{/if}
			{/snippet}
			{#snippet actions()}
				{#if canPlayAll}
					<Button
						variant="primary"
						icon={Play}
						class="min-w-28 [&_svg]:fill-current"
						loading={allPlayback.loading}
						aria-label="Play all tracks from {artist.name}"
						onclick={() => allPlayback.play()}>Play</Button
					>
					<Button
						variant="outline"
						icon={Shuffle}
						disabled={allPlayback.loading}
						aria-label="Shuffle all tracks from {artist.name}"
						onclick={() => allPlayback.shuffle()}
						><span class="max-sm:sr-only">Shuffle</span></Button
					>
				{/if}
				<div class="dn-artist-follow">
					<FollowControl artistMbid={artist.musicbrainz_id} size="md" />
				</div>
				{#if localArtist}
					<FavoriteHeart
						size="md"
						favorite={localArtist.favorite ?? false}
						onchange={(favorited) =>
							heart.mutate({ kind: 'artist', itemId: localArtist.id, favorited })}
					/>
				{/if}
				{#if localArtist}
					<RatingStars
						size="md"
						class="dn-artist-stars"
						value={ratingOf(ratingsQuery.data, 'artist', localArtist.id)}
						label={artist.name}
						onchange={(rating) => rate.mutate({ kind: 'artist', itemId: localArtist.id, rating })}
					/>
				{/if}
				{#if menuItems.length > 0}
					<ContextMenu items={menuItems} trigger="button" label="More artist actions" />
				{/if}
			{/snippet}
			{#snippet below()}
				{#if canPlayAll}
					<PlayFromChooser
						sources={playSource.available}
						selected={playSource.selected}
						onchoose={(source) => playSource.choose(source)}
					/>
				{/if}
				{#if allPlayback.progressText}
					<p class="dn-artist-note animate-pulse" aria-live="polite">{allPlayback.progressText}</p>
				{/if}
				{#if refreshingArtist}
					<p class="dn-artist-note" aria-live="polite">Refreshing artist info…</p>
				{/if}
			{/snippet}
		</ArtistHero>

		<JumpTabs sections={tocSections} label="Artist page sections" />

		<div class="dn-artist-page space-y-10">
			<div class="dn-artist-row" class:dn-artist-row--pair={ownsAlbums}>
				{#if ownsAlbums && localArtist}
					<div class="dn-artist-stack">
						<ArtistLibraryAlbums
							libraryArtistId={localArtist.id}
							artistMbid={data.artistId}
							onplay={playSource.available.length > 0
								? (album, kind) => {
										targetAlbum(album, kind);
										albumPlayback.play();
									}
								: undefined}
							playing={albumPlayback.loading}
							menu={albumMenu}
						/>
						{#if genreTags.length > 0}<ArtistGenres tags={genreTags} />{/if}
						{#if noService}<ArtistDiscoverMore
								artistName={artist.name}
								turnedOff={recommendationsOff}
							/>{/if}
					</div>
				{/if}
				<ArtistAbout
					name={artist.name}
					description={artist.description}
					descriptionLoading={loadingExtended && !artistExtended}
					lastfm={lastfmEnrichment}
					lastfmLoading={loadingLastfm}
					facts={aboutFacts}
					links={artist.external_links.filter((link) => link.url && link.url.trim() !== '')}
					{musicbrainzUrl}
				>
					{#snippet footer()}
						{#if localArtist}
							<ArtistMusicBrainzLink artistId={localArtist.id} />
						{/if}
						<ArtistWhereToBuy artistMbid={data.artistId} artistName={artist.name} />
					{/snippet}
				</ArtistAbout>
			</div>

			{#if artist.appears_in_library}
				<ArtistAppearancesSection artistId={data.artistId} />
			{/if}

			{#if noService && !ownsAlbums}
				<div class="dn-artist-row" class:dn-artist-row--wide={genresBelow}>
					<ArtistDiscoverMore artistName={artist.name} turnedOff={recommendationsOff} />
					{#if genresBelow}<ArtistGenres tags={genreTags} />{/if}
				</div>
			{:else if !noService}
				<section id="section-popular" class="scroll-mt-32" aria-labelledby="artist-popular-title">
					<div class="dn-artist-head">
						<h2 id="artist-popular-title" class="dn-artist-head__title">Popular</h2>
						{#if linkedSources.length === 2}
							<SimpleSourceSwitcher
								currentSource={resolvedSource}
								onSourceChange={(newSource) => {
									activeSource.current = newSource;
								}}
							/>
						{/if}
					</div>

					{#if sourceNotice}
						<div
							role="status"
							class="alert alert-warning alert-soft mb-4 flex items-center gap-2 text-sm"
						>
							<CircleAlert class="h-4 w-4 shrink-0" />
							<span>{sourceNotice}</span>
						</div>
					{/if}

					<Card variant="bordered" padding="none">
						<div class="dn-artist-popular">
							<div bind:this={albumsDemandEl} class="min-w-0">
								<TopAlbumsList
									albums={topAlbums?.albums || []}
									loading={loadingTopAlbums}
									configured={topAlbums?.configured ?? true}
									source={topAlbums?.source || ''}
								/>
							</div>
							<div bind:this={songsDemandEl} class="min-w-0">
								<TopSongsList
									songs={topSongs?.songs || []}
									loading={loadingTopSongs}
									configured={topSongs?.configured ?? true}
									source={topSongs?.source || ''}
								/>
							</div>
						</div>
					</Card>
				</section>

				<div class="dn-artist-row" class:dn-artist-row--wide={genresBelow}>
					<section
						bind:this={similarDemandEl}
						id="section-similar"
						class="min-w-0 scroll-mt-32"
						aria-label="Similar artists"
					>
						<SectionHead title="Similar artists" />
						<SimilarArtistsCarousel
							artists={similarArtists?.similar_artists || []}
							loading={loadingSimilar}
							configured={similarArtists?.configured ?? true}
							source={similarArtists?.source}
						/>
					</section>
					{#if genresBelow}<ArtistGenres tags={genreTags} class="self-start" />{/if}
				</div>
			{/if}

			{#if initialReleasesLoading}
				<AlbumGridSkeleton title="Discography" count={8} />
			{:else}
				<ArtistDiscography
					albums={releases.albums}
					eps={releases.eps}
					singles={releases.singles}
					artistName={artist.name}
					requestingIds={requestedReleaseIds}
					loadingMore={loadingMoreReleases}
					hasMore={hasMoreReleases}
					loadedCount={loadedReleaseCount}
					totalCount={sourceTotalCount}
					onrequest={handleRequest}
					onremoved={handleReleaseRemoved}
					ondownloadall={openDownloadModal}
					onloadmore={handleLoadMoreReleases}
				/>
			{/if}
		</div>
	{:else}
		<div class="dn-artist-page flex min-h-[50vh] items-center justify-center">
			<p class="text-muted">Artist not found</p>
		</div>
	{/if}
</div>

<style>
	.dn-artist-page {
		width: 100%;
		max-width: 80rem;
		margin: 0 auto;
		padding: 1.5rem 1rem 2rem;
	}

	.dn-artist-row {
		display: grid;
		align-items: start;
		gap: 1.5rem;
	}
	@media (min-width: 1280px) {
		.dn-artist-row--pair {
			grid-template-columns: minmax(0, 1.08fr) minmax(0, 1fr);
			/* About runs as tall as the library column beside it */
			align-items: stretch;
		}
	}
	@media (min-width: 1024px) {
		.dn-artist-row--wide {
			grid-template-columns: minmax(0, 1.45fr) minmax(0, 1fr);
		}
	}

	/* the library card, then the genres and Discover more, beside About */
	.dn-artist-stack {
		display: flex;
		min-width: 0;
		flex-direction: column;
		gap: 1.5rem;
	}

	.dn-artist-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		margin-bottom: 1rem;
	}
	.dn-artist-head__title {
		font-family: var(--font-display);
		font-size: clamp(2rem, 1.5rem + 1.5vw, 2.75rem);
		line-height: 1;
		color: var(--color-base-content);
	}

	.dn-artist-popular {
		display: grid;
		gap: 1.5rem;
		padding: 1.25rem;
	}
	@media (min-width: 768px) {
		.dn-artist-popular {
			grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
			gap: 2rem;
			padding: 1.5rem;
		}
		.dn-artist-popular > :last-child {
			padding-left: 2rem;
			border-left: 1px solid var(--dn-line);
		}
	}

	/* Follow sits in the row of buttons; its auto-download switch drops to
	   the line under them. */
	.dn-artist-follow,
	.dn-artist-follow > :global(div) {
		display: contents;
	}
	.dn-artist-follow > :global(div > div) {
		order: 20;
		flex-basis: 100%;
		padding-left: 0;
	}
	@media (max-width: 639px) {
		:global(.dn-artist-stars) {
			order: 10;
			flex-basis: 100%;
			margin-left: -0.25rem;
		}
	}

	.dn-artist-note {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.08em;
		color: var(--dn-muted);
	}

	@media (min-width: 768px) {
		.dn-artist-page {
			padding-inline: 1.5rem;
		}
	}
	@media (min-width: 1024px) {
		.dn-artist-page {
			padding-inline: 2rem;
		}
	}
</style>
