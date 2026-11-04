<script lang="ts">
	import { goto } from '$app/navigation';
	import {
		getRemoteArtistIndexQuery,
		getRemoteFavoritesQuery,
		getRemoteHubQuery,
		refreshRemoteSource,
		remoteErrorCode
	} from '$lib/features/remote-library/queries/RemoteQueries.svelte';
	import type { RemoteAlbum, BrowseHeroCard } from '$lib/features/remote-library/queries/types';
	import { getSourcePlaylistsQuery } from '$lib/features/remote-library/queries/source-playlists.svelte';
	import AlbumGrid from '$lib/features/remote-library/components/AlbumGrid.svelte';
	import ArtistIndexSidebar from '$lib/features/remote-library/components/ArtistIndexSidebar.svelte';
	import BrowseHeroCards from '$lib/features/remote-library/components/BrowseHeroCards.svelte';
	import FeaturedAlbumHero from '$lib/features/remote-library/components/FeaturedAlbumHero.svelte';
	import HubPageSkeleton from '$lib/features/remote-library/components/HubPageSkeleton.svelte';
	import HubShelf from '$lib/features/remote-library/components/HubShelf.svelte';
	import MostPlayedSection from '$lib/features/remote-library/components/MostPlayedSection.svelte';
	import NowPlayingWidget from '$lib/features/player/components/NowPlayingWidget.svelte';
	import PlaylistImportBanner from '$lib/features/remote-library/components/PlaylistImportBanner.svelte';
	import SourceAlbumModal from '$lib/features/remote-library/components/SourceAlbumModal.svelte';
	import SourceHubHeader from '$lib/features/remote-library/components/SourceHubHeader.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { reveal } from '$lib/actions/reveal';
	import { nowPlayingMerged } from '$lib/features/player/state/nowPlayingMerged.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import AlbumCarousel from '$lib/features/remote-library/components/AlbumCarousel.svelte';
	import BrandLine from '$lib/features/remote-library/components/BrandLine.svelte';
	import HubArtistShelves from '$lib/features/remote-library/components/HubArtistShelves.svelte';
	import HubDiscovery from '$lib/features/remote-library/components/HubDiscovery.svelte';
	import HubFavorites from '$lib/features/remote-library/components/HubFavorites.svelte';
	import HubHistory from '$lib/features/remote-library/components/HubHistory.svelte';
	import HubSimilarAlbums from '$lib/features/remote-library/components/HubSimilarAlbums.svelte';
	import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
	const remote = $derived(data.remote);
	const caps = $derived(remote.capabilities);
	const base = $derived(`/library/${remote.source}`);

	let selectedAlbum = $state<RemoteAlbum | null>(null);
	let modalOpen = $state(false);
	let refreshing = $state(false);

	// A source that is off answers nothing useful, so it is not asked.
	const integrations = useIntegrationStatus();
	const sourceOn = $derived(integrations.current[remote.source]);
	const sourceOff = $derived(integrations.current.loaded && !sourceOn);

	const hubQuery = getRemoteHubQuery(
		() => remote.source,
		() => sourceOn
	);
	const favoritesQuery = getRemoteFavoritesQuery(
		() => remote.source,
		() => caps.favorites && sourceOn
	);
	const artistIndexQuery = getRemoteArtistIndexQuery(
		() => remote.source,
		() => sourceOn
	);
	const playlistsQuery = getSourcePlaylistsQuery(
		() => remote.source,
		undefined,
		() => sourceOn
	);

	const hub = $derived(hubQuery.data ?? null);
	const playlists = $derived(playlistsQuery.data);
	const playlistErrorCode = $derived(
		playlistsQuery.isError
			? remoteErrorCode(playlistsQuery.error, 'SOURCE_PLAYLISTS_UNAVAILABLE')
			: ''
	);
	const sessions = $derived(nowPlayingMerged.sessionsForSource(remote.source));

	const artistIndex = $derived(artistIndexQuery.data?.index ?? []);

	const browseCards = $derived<BrowseHeroCard[]>([
		{
			label: 'Albums',
			value: hub?.stats?.total_albums ?? null,
			href: withBasePath(`${base}/albums`),
			subtitle: 'in your library',
			colorScheme: 'primary',
			icon: 'disc'
		},
		{
			label: 'Artists',
			value: hub?.stats?.total_artists ?? null,
			href: withBasePath(`${base}/artists`),
			subtitle: 'in your library',
			colorScheme: 'secondary',
			icon: 'users'
		},
		{
			label: 'Tracks',
			value: hub?.stats?.total_tracks ?? null,
			href: withBasePath(`${base}/tracks`),
			subtitle: 'in your library',
			colorScheme: 'accent',
			icon: 'music'
		}
	]);

	const recentlyAddedHref = $derived(
		withBasePath(`${base}/albums?sort=${remote.sorts.albumsAdded}`)
	);

	async function refreshHub() {
		refreshing = true;
		try {
			await Promise.all([refreshRemoteSource(remote.source), playlistsQuery.refetch()]);
		} finally {
			refreshing = false;
		}
	}

	function openAlbum(album: RemoteAlbum) {
		selectedAlbum = album;
		modalOpen = true;
	}
</script>

<div class="container mx-auto space-y-6 p-6">
	<BrandLine {remote} />

	<SourceHubHeader
		title="{remote.label} Library"
		albumCount={hub?.stats?.total_albums ?? null}
		onrefresh={refreshHub}
		{refreshing}
	>
		{#snippet icon()}
			<remote.icon class="h-8 w-8 {remote.tone}" />
		{/snippet}
	</SourceHubHeader>

	{#if sourceOff}
		<p class="text-sm text-muted">{remote.label} isn't set up.</p>
	{:else}
		<BrowseHeroCards cards={browseCards} />

		<PlaylistImportBanner
			playlists={playlists?.playlists ?? []}
			accountMode={playlists?.account_mode}
			accountLabel={playlists?.account_label}
			loading={playlistsQuery.isPending}
			errorCode={playlistErrorCode}
			onretry={() => void playlistsQuery.refetch()}
			sourceLabel={remote.label}
			playlistsHref={withBasePath(`${base}/playlists`)}
		>
			{#snippet sourceIcon()}
				<remote.icon class="h-4 w-4 {remote.tone}" />
			{/snippet}
		</PlaylistImportBanner>

		<NowPlayingWidget {sessions} />

		<QueryBoundary query={hubQuery} what="{remote.label} library">
			{#snippet loading()}
				<HubPageSkeleton />
			{/snippet}
			{#snippet children(hub)}
				<FeaturedAlbumHero albums={hub.recently_played} onAlbumClick={openAlbum} />

				{#if caps.similarAlbums}
					<HubSimilarAlbums
						{remote}
						seed={hub.recently_played[0] ?? favoritesQuery.data?.albums[0] ?? null}
						onopen={openAlbum}
					/>
				{/if}

				{#if caps.favorites}
					<HubFavorites
						favorites={favoritesQuery.data}
						loading={favoritesQuery.isFetching}
						onopen={openAlbum}
					/>
				{/if}

				<HubDiscovery {remote} genres={hub.genres} onopen={openAlbum} />

				{#if caps.recentlyAdded}
					<div use:reveal>
						<HubShelf title="Recently Added" seeAllHref={recentlyAddedHref}>
							{#if hub.recently_added.length > 0}
								<AlbumGrid
									albums={hub.recently_added}
									seeAllHref={recentlyAddedHref}
									onAlbumClick={openAlbum}
								/>
							{:else}
								<p class="text-sm text-muted">Nothing new yet.</p>
							{/if}
						</HubShelf>
					</div>
				{/if}

				{#if caps.artistInfo}
					<HubArtistShelves
						{remote}
						artist={hub.favorite_artists[0] ?? null}
						track={favoritesQuery.data?.tracks[0] ?? null}
					/>
				{/if}

				{#if caps.playCounts && hub.most_played_artists.length > 0}
					<div use:reveal>
						<HubShelf title="Most Played">
							<MostPlayedSection
								artists={hub.most_played_artists.map((a) => ({
									id: a.id,
									name: a.name,
									image_url: a.image_url,
									musicbrainz_id: a.artist_mbid,
									album_count: a.album_count ?? undefined
								}))}
								albums={[]}
							/>
						</HubShelf>
					</div>
				{/if}

				{#if caps.activity}
					<div class="section-divider-glow"></div>
					<HubHistory {remote} />
				{/if}

				<div class="section-divider-glow"></div>

				<div class="mt-10 mb-6 rounded-2xl bg-base-200/20 p-6 space-y-6">
					<HubShelf title="Artists A-Z" loading={artistIndexQuery.isFetching}>
						{#if artistIndex.length > 0}
							<ArtistIndexSidebar
								index={artistIndex}
								onselect={(artist) => {
									if (artist.artist_mbid) {
										goto(withBasePath(`/artist/${artist.artist_mbid}`));
									} else {
										goto(
											withBasePath(
												`${base}/${remote.artistSearch}?search=${encodeURIComponent(artist.name)}`
											)
										);
									}
								}}
							/>
						{:else if artistIndexQuery.isError}
							<p class="text-sm text-muted">Couldn't load the artists.</p>
						{:else if !artistIndexQuery.isFetching}
							<p class="text-sm text-muted">No artists found.</p>
						{/if}
					</HubShelf>

					<HubShelf title="Browse Albums" seeAllHref={withBasePath(`${base}/albums`)}>
						{#if hub.all_albums_preview.length > 0}
							<AlbumCarousel albums={hub.all_albums_preview} onopen={openAlbum} />
						{:else}
							<p class="text-sm text-muted">No albums found.</p>
						{/if}
					</HubShelf>
				</div>
			{/snippet}
		</QueryBoundary>
	{/if}
</div>

<SourceAlbumModal
	bind:open={modalOpen}
	sourceType={remote.source}
	album={selectedAlbum}
	onclose={() => {
		modalOpen = false;
		selectedAlbum = null;
	}}
/>
