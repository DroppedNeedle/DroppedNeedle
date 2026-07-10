<script lang="ts">
	import type { ReleaseGroup } from '$lib/types';
	import { dedupeById } from '$lib/utils/dedupe';
	import ArtistHeaderSkeleton from '$lib/components/ArtistHeaderSkeleton.svelte';
	import AlbumGridSkeleton from '$lib/components/AlbumGridSkeleton.svelte';
	import ArtistWhereToBuy from '$lib/components/ArtistWhereToBuy.svelte';
	import ReleaseList from '$lib/components/ReleaseList.svelte';
	import Toast from '$lib/components/Toast.svelte';
	import ArtistHero from '$lib/components/ArtistHero.svelte';
	import ArtistDescription from '$lib/components/ArtistDescription.svelte';
	import SimilarArtistsCarousel from '$lib/components/SimilarArtistsCarousel.svelte';
	import TopSongsList from '$lib/components/TopSongsList.svelte';
	import TopAlbumsList from '$lib/components/TopAlbumsList.svelte';
	import ArtistRemovedModal from '$lib/components/ArtistRemovedModal.svelte';
	import LastFmEnrichment from '$lib/components/LastFmEnrichment.svelte';
	import LibraryAlbumsCarousel from '$lib/components/LibraryAlbumsCarousel.svelte';
	import ArtistPageToc from '$lib/components/ArtistPageToc.svelte';
	import { requestAlbum } from '$lib/utils/albumRequest';
	import { libraryStore } from '$lib/stores/library';
	import { type MusicSource, isMusicSource } from '$lib/stores/musicSource';
	import {
		getArtistLastFmEnrichmentQuery,
		getArtistReleasesInfiniteQuery,
		getArtistTopAlbumsQuery,
		getArtistTopSongsQuery,
		getBasicArtistQuery,
		getExtendedArtistQuery,
		getSimilarArtistsQuery,
		updateArtistReleaseInCache
	} from '$lib/queries/artist/ArtistQueries.svelte';
	import type { PageProps } from './$types';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
	import { PAGE_SOURCE_KEYS } from '$lib/constants';
	import { PersistedState } from 'runed';
	import SimpleSourceSwitcher from '$lib/components/SimpleSourceSwitcher.svelte';
	import ArtistPlaybackBar from '$lib/components/ArtistPlaybackBar.svelte';
	import {
		discographyDownloadStore,
		type DiscographyRelease
	} from '$lib/stores/discographyDownload.svelte';
	import { CalendarDays, Disc3, Download, Globe2, Loader2, Sparkles } from 'lucide-svelte';
	import { goto } from '$app/navigation';
	import { createGenerateSmartMixMutation } from '$lib/queries/playlists/PlaylistMutations.svelte';
	import { toastStore } from '$lib/stores/toast';

	let { data }: PageProps = $props();

	// svelte-ignore state_referenced_locally
	let activeSource = new PersistedState<MusicSource>(
		PAGE_SOURCE_KEYS['artist'],
		data.primarySource
	);

	let validSource = $derived(
		isMusicSource(activeSource.current) ? activeSource.current : data.primarySource
	);

	let showToast = $state(false);
	let toastMessage = 'Added to Library';
	let showArtistRemovedModal = $state(false);
	let removedArtistName = $state('');
	let requestedReleaseIds = $state(new Set<string>());
	let albumsCollapsed = $state(false);
	let epsCollapsed = $state(false);
	let singlesCollapsed = $state(false);

	type ArtistTocSection = {
		id: string;
		label: string;
	};

	const artistBasicQuery = getBasicArtistQuery(() => data.artistId);
	const artistBasic = $derived(artistBasicQuery.data);
	const loadingBasic = $derived(artistBasicQuery.isLoading);

	const artistExtendedQuery = getExtendedArtistQuery(() => data.artistId);
	const artistExtended = $derived(artistExtendedQuery.data);
	const loadingExtended = $derived(artistExtendedQuery.isLoading);

	const similarArtistsQuery = getSimilarArtistsQuery(() => ({
		artistId: data.artistId,
		source: validSource
	}));
	const similarArtists = $derived(similarArtistsQuery.data);
	const loadingSimilar = $derived(similarArtistsQuery.isLoading);

	const topSongsQuery = getArtistTopSongsQuery(() => ({
		artistId: data.artistId,
		source: validSource
	}));
	const topSongs = $derived(topSongsQuery.data);
	const loadingTopSongs = $derived(topSongsQuery.isLoading);

	const topAlbumsQuery = getArtistTopAlbumsQuery(() => ({
		artistId: data.artistId,
		source: validSource
	}));
	const topAlbums = $derived(topAlbumsQuery.data);
	const loadingTopAlbums = $derived(topAlbumsQuery.isLoading);

	const lastFmEnrichmentQuery = getArtistLastFmEnrichmentQuery(() => ({
		artistId: data.artistId,
		artistName: artistBasic?.name
	}));
	const lastfmEnrichment = $derived(lastFmEnrichmentQuery.data);
	const loadingLastfm = $derived(lastFmEnrichmentQuery.isLoading);

	let error: string | null = $derived.by(() => {
		if (artistBasicQuery.error) {
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
			description: artistExtended?.description,
			image: artistExtended?.image
		};
	});

	const releasesQuery = getArtistReleasesInfiniteQuery(() => data.artistId);
	const loadingMoreReleases = $derived(releasesQuery.isFetchingNextPage);
	const hasMoreReleases = $derived(releasesQuery.hasNextPage);
	const releases = $derived.by(() => {
		const albums = releasesQuery.data?.pages.flatMap((page) => page.albums) || [];
		const singles = releasesQuery.data?.pages.flatMap((page) => page.singles) || [];
		const eps = releasesQuery.data?.pages.flatMap((page) => page.eps) || [];
		// pages can repeat a release group; dedupe so the keyed {#each} never hits each_key_duplicate, which would blank the page
		return {
			albums: sortReleasesByYear(dedupeById(albums)),
			singles: sortReleasesByYear(dedupeById(singles)),
			eps: sortReleasesByYear(dedupeById(eps))
		};
	});
	const loadedReleaseCount = $derived(
		releasesQuery.data?.pages.flatMap((page) => [...page.albums, ...page.singles, ...page.eps])
			.length || 0
	);
	const initialReleasesLoading = $derived(releasesQuery.isLoading);
	const sourceTotalCount = $derived(releasesQuery.data?.pages[0]?.source_total_count ?? null);

	$effect(() => {
		if (hasMoreReleases && !releasesQuery.isFetchingNextPage) {
			releasesQuery.fetchNextPage();
		}
	});

	const refreshingArtist = $derived(
		artistBasicQuery.isRefetching || artistExtendedQuery.isRefetching
	);

	function sortReleasesByYear(releases: ReleaseGroup[]) {
		return [...releases].sort((a, b) => {
			const yearA = a.year;
			const yearB = b.year;
			if (yearA === null || yearA === undefined) return 1;
			if (yearB === null || yearB === undefined) return -1;
			return yearB - yearA;
		});
	}

	async function handleRefreshClick() {
		invalidateQueriesWithPersister({ queryKey: ArtistQueryKeyFactory.basic(data.artistId) });
	}

	async function handleRequest(releaseId: string, releaseTitle?: string) {
		requestedReleaseIds.add(releaseId);
		requestedReleaseIds = requestedReleaseIds;

		try {
			const result = await requestAlbum(releaseId, {
				artist: artist?.name,
				album: releaseTitle
			});

			if (result.success && artist) {
				await updateArtistReleaseInCache(data.artistId, {
					id: releaseId,
					requested: true
				});

				showToast = true;
			}
		} finally {
			requestedReleaseIds.delete(releaseId);
			requestedReleaseIds = requestedReleaseIds;
		}
	}

	function handleReleaseRemoved(result: { artist_removed: boolean; artist_name?: string | null }) {
		if (!artist) return;

		if (result.artist_removed) {
			artist.in_library = false;
			removedArtistName = result.artist_name || artist.name;
			showArtistRemovedModal = true;
		}
		invalidateQueriesWithPersister({ queryKey: ArtistQueryKeyFactory.basic(data.artistId) });
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

	function openDiscographyModal(releasesToShow?: typeof allReleases) {
		if (!artist) return;
		const items: DiscographyRelease[] = (releasesToShow ?? allReleases).map((r) => ({
			id: r.id,
			title: r.title,
			type: r.type ?? 'Album',
			year: r.year,
			in_library: libraryStore.isInLibrary(r.id) || (!$libraryStore.initialized && r.in_library),
			requested: r.requested || libraryStore.isRequested(r.id)
		}));
		discographyDownloadStore.show(artist.name, data.artistId, items);
	}

	function openSectionDownloadModal(sectionReleases: typeof allReleases, type: string) {
		if (!artist) return;
		const items: DiscographyRelease[] = sectionReleases.map((r) => ({
			id: r.id,
			title: r.title,
			type,
			year: r.year,
			in_library: libraryStore.isInLibrary(r.id) || (!$libraryStore.initialized && r.in_library),
			requested: r.requested || libraryStore.isRequested(r.id)
		}));
		discographyDownloadStore.show(artist.name, data.artistId, items);
	}

	const smartMixMutation = createGenerateSmartMixMutation();

	async function handleSmartMix() {
		if (!artist || smartMixMutation.isPending) return;
		try {
			const created = await smartMixMutation.mutateAsync({
				seeds: [{ type: 'artist', value: data.artistId }],
				count: 25,
				name: `${artist.name} — Smart Mix`
			});
			toastStore.show({ message: `Smart Mix created from ${artist.name}`, type: 'success' });
			await goto(`/playlists/${created.id}`);
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : "Couldn't create the Smart Mix",
				type: 'error'
			});
		}
	}

	const tocSections = $derived.by<ArtistTocSection[]>(() => {
		if (!artist) {
			return [];
		}
		return [
			{ id: 'section-overview', label: 'Overview' },
			{ id: 'section-about', label: 'About' },
			{ id: 'section-similar', label: 'Similar Artists' },
			...(releases.albums.length > 0 ? [{ id: 'section-albums', label: 'Albums' }] : []),
			...(releases.eps.length > 0 ? [{ id: 'section-eps', label: 'EPs' }] : []),
			...(releases.singles.length > 0 ? [{ id: 'section-singles', label: 'Singles' }] : [])
		];
	});
</script>

<div class="w-full px-2 sm:px-4 lg:px-8 py-4 sm:py-8 max-w-7xl mx-auto">
	{#if error}
		<div class="flex items-center justify-center min-h-[50vh]">
			<div
				class="flex items-center gap-3 rounded-2xl border border-error/30 bg-error/10 px-5 py-4 text-sm text-error"
			>
				<span>{error}</span>
			</div>
		</div>
	{:else if loadingBasic && !artist}
		<div class="space-y-4 sm:space-y-8">
			<ArtistHeaderSkeleton />
			<AlbumGridSkeleton title="Albums" count={12} />
		</div>
	{:else if artist}
		<div class="xl:grid xl:grid-cols-[10rem_minmax(0,1fr)] xl:gap-6">
			<ArtistPageToc sections={tocSections} />

			<div class="xl:col-start-2 xl:row-start-1 space-y-4 sm:space-y-6 lg:space-y-8">
				<section id="section-overview" class="space-y-4 scroll-mt-24">
					<ArtistHero
						{artist}
						showBackButton
						refreshing={refreshingArtist}
						onrefresh={handleRefreshClick}
					/>

					<div
						class="flex flex-wrap items-center justify-center gap-x-5 gap-y-2 text-sm text-base-content/60 sm:justify-start"
					>
						{#if artist.country}
							<span class="flex items-center gap-1.5">
								<Globe2 class="h-3.5 w-3.5 text-accent" />
								{artist.country}
							</span>
						{/if}
						{#if artist.life_span?.begin}
							<span class="flex items-center gap-1.5">
								<CalendarDays class="h-3.5 w-3.5 text-accent" />
								{artist.life_span.begin}
								{#if artist.life_span.end}
									&nbsp;–&nbsp;
									{artist.life_span.end}
								{/if}
							</span>
						{/if}
						{#if releases.albums.length + releases.eps.length + releases.singles.length > 0}
							<span class="flex items-center gap-1.5 font-mono tabular-nums">
								<Disc3 class="h-3.5 w-3.5 text-accent" />
								{releases.albums.length + releases.eps.length + releases.singles.length} releases
							</span>
						{/if}
					</div>

					{#if artist.tags.length > 0}
						<div class="-mt-1 flex flex-wrap justify-center gap-2 sm:justify-start">
							{#each [...new Set(artist.tags)].slice(0, 10) as tag (tag)}
								<a
									href="/genre?name={encodeURIComponent(tag)}"
									class="rounded-full border border-base-content/10 bg-base-200/50 px-3 py-1 text-xs font-medium text-base-content/70 transition-colors hover:border-primary/40 hover:bg-base-200 hover:text-base-content"
									>{tag}</a
								>
							{/each}
						</div>
					{/if}

					<ArtistWhereToBuy artistMbid={data.artistId} artistName={artist.name} />
				</section>

				<section id="section-about" class="space-y-4 scroll-mt-24">
					{#if !lastfmEnrichment?.bio}
						<ArtistDescription description={artist.description} loading={loadingExtended} />
					{/if}

					{#if loadingLastfm || lastfmEnrichment}
						<LastFmEnrichment enrichment={lastfmEnrichment} loading={loadingLastfm} />
					{/if}
				</section>

				<ArtistPlaybackBar
					artistName={artist.name}
					artistId={data.artistId}
					releases={[...releases.albums, ...releases.eps, ...releases.singles]}
				/>

				<div class="flex flex-wrap items-center justify-center gap-2 sm:justify-start">
					{#if downloadableReleaseCount > 0}
						<button
							class="btn btn-primary btn-sm gap-1.5 rounded-full"
							onclick={() => openDiscographyModal()}
						>
							<Download class="h-4 w-4" />
							Download Discography
							<span
								class="ml-0.5 rounded-full bg-primary-content/15 px-2 py-0.5 font-mono text-[0.68rem] font-bold tabular-nums"
								>{downloadableReleaseCount}</span
							>
						</button>
					{/if}
					<button
						class="btn btn-ghost btn-sm gap-1.5 rounded-full border border-base-content/10"
						disabled={smartMixMutation.isPending}
						aria-label="Create a Smart Mix playlist seeded from {artist.name}"
						onclick={() => void handleSmartMix()}
					>
						{#if smartMixMutation.isPending}
							<Loader2 class="h-4 w-4 animate-spin" />
						{:else}
							<Sparkles class="h-4 w-4 text-accent" />
						{/if}
						Smart Mix
					</button>
				</div>

				<LibraryAlbumsCarousel
					releases={[...releases.albums, ...releases.eps, ...releases.singles]}
					artistName={artist.name}
					loading={loadingBasic}
				/>

				<div class="mt-8 mb-4 flex items-center justify-end">
					<SimpleSourceSwitcher
						currentSource={validSource}
						onSourceChange={(newSource) => {
							activeSource.current = newSource;
						}}
					/>
				</div>

				<div
					class="flex flex-col gap-6 rounded-2xl border border-base-content/8 bg-base-200/50 p-4 sm:p-5 md:flex-row md:items-stretch"
				>
					<div class="flex-1 min-w-0">
						<TopAlbumsList
							albums={topAlbums?.albums || []}
							loading={loadingTopAlbums}
							configured={topAlbums?.configured ?? true}
							source={topAlbums?.source || ''}
						/>
					</div>
					<div
						class="h-px w-full shrink-0 bg-base-content/10 md:h-auto md:w-px md:self-stretch"
						aria-hidden="true"
					></div>
					<div class="flex-1 min-w-0">
						<TopSongsList
							songs={topSongs?.songs || []}
							loading={loadingTopSongs}
							configured={topSongs?.configured ?? true}
							source={topSongs?.source || ''}
						/>
					</div>
				</div>

				<section id="section-similar" class="mt-8 scroll-mt-24">
					<SimilarArtistsCarousel
						artists={similarArtists?.similar_artists || []}
						loading={loadingSimilar}
						configured={similarArtists?.configured ?? true}
					/>
				</section>

				{#if initialReleasesLoading}
					<AlbumGridSkeleton title="Discography" count={12} />
				{:else if hasMoreReleases || loadingMoreReleases}
					<div
						class="mb-6 flex items-center justify-center gap-3 rounded-2xl border border-accent/25 bg-base-200/50 p-4"
					>
						<span class="loading loading-spinner loading-md text-accent"></span>
						<div class="flex flex-col items-start">
							<span
								class="font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-accent"
								>Loading releases…</span
							>
							<span class="text-sm text-base-content/60 tabular-nums"
								>{#if sourceTotalCount}Loaded {loadedReleaseCount} of {sourceTotalCount} releases{:else}Loading
									{loadedReleaseCount} releases{/if}</span
							>
						</div>
					</div>
				{/if}

				{#if releases.albums.length > 0}
					<section id="section-albums" class="scroll-mt-24">
						<ReleaseList
							title="Albums"
							releases={releases.albums}
							collapsed={albumsCollapsed}
							requestingIds={requestedReleaseIds}
							showLoadingIndicator={hasMoreReleases || loadingMoreReleases}
							artistName={artist.name}
							onRequest={handleRequest}
							onRemoved={handleReleaseRemoved}
							onToggleCollapse={() => (albumsCollapsed = !albumsCollapsed)}
							onDownloadAll={() => openSectionDownloadModal(releases.albums, 'Album')}
						/>
					</section>
				{/if}

				{#if releases.eps.length > 0}
					<section id="section-eps" class="scroll-mt-24">
						<ReleaseList
							title="EPs"
							releases={releases.eps}
							collapsed={epsCollapsed}
							requestingIds={requestedReleaseIds}
							showLoadingIndicator={hasMoreReleases || loadingMoreReleases}
							artistName={artist.name}
							onRequest={handleRequest}
							onRemoved={handleReleaseRemoved}
							onToggleCollapse={() => (epsCollapsed = !epsCollapsed)}
							onDownloadAll={() => openSectionDownloadModal(releases.eps, 'EP')}
						/>
					</section>
				{/if}

				{#if releases.singles.length > 0}
					<section id="section-singles" class="scroll-mt-24">
						<ReleaseList
							title="Singles"
							releases={releases.singles}
							collapsed={singlesCollapsed}
							requestingIds={requestedReleaseIds}
							showLoadingIndicator={hasMoreReleases || loadingMoreReleases}
							artistName={artist.name}
							onRequest={handleRequest}
							onRemoved={handleReleaseRemoved}
							onToggleCollapse={() => (singlesCollapsed = !singlesCollapsed)}
							onDownloadAll={() => openSectionDownloadModal(releases.singles, 'Single')}
						/>
					</section>
				{/if}
			</div>
		</div>
	{:else}
		<div class="flex items-center justify-center min-h-[50vh]">
			<p class="text-base-content/60">Artist not found</p>
		</div>
	{/if}
</div>

<Toast bind:show={showToast} message={toastMessage} />

{#if showArtistRemovedModal}
	<ArtistRemovedModal
		artistName={removedArtistName}
		onclose={() => {
			showArtistRemovedModal = false;
		}}
	/>
{/if}
