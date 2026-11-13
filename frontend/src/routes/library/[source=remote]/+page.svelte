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
	import HubNowPlaying from '$lib/features/remote-library/components/HubNowPlaying.svelte';
	import HubNotSetUp from '$lib/features/remote-library/components/HubNotSetUp.svelte';
	import { playRemoteAlbum } from '$lib/features/remote-library/components/playRemoteAlbum';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { CircleDashed, Disc3, LibraryBig } from 'lucide-svelte';
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
			icon: 'disc'
		},
		{
			label: 'Artists',
			value: hub?.stats?.total_artists ?? null,
			href: withBasePath(`${base}/artists`),
			subtitle: 'in your library',
			icon: 'users'
		},
		{
			label: 'Tracks',
			value: hub?.stats?.total_tracks ?? null,
			href: withBasePath(`${base}/tracks`),
			subtitle: 'in your library',
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

	// The album dialog says what went wrong when the tracks cannot be read or played.
	async function continueAlbum(album: RemoteAlbum) {
		const played = await playRemoteAlbum(album).catch(() => false);
		if (!played) openAlbum(album);
	}

	// Plex's amber takes dark ink; the others light.
	const brandInk = $derived(
		remote.source === 'plex' ? 'var(--dn-action-content)' : 'var(--dn-paper)'
	);
</script>

<svelte:head><title>{remote.label} - DroppedNeedle</title></svelte:head>

<div
	class="hub"
	style:--hub-brand="rgb(var(--brand-{remote.source}))"
	style:--hub-brand-ink={brandInk}
>
	<BrandLine {remote} class="hub__line" />

	<SourceHubHeader
		{remote}
		albumCount={hub?.stats?.total_albums ?? null}
		off={sourceOff}
		onrefresh={refreshHub}
		{refreshing}
	/>

	<div class="hub__body">
		{#if sourceOff}
			<HubNotSetUp {remote} isAdmin={authStore.isAdmin} />
		{:else}
			<div class="hub__top">
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
			</div>

			{#if !hub}
				<HubNowPlaying {sessions} sourceLabel={remote.label} />
			{/if}

			<QueryBoundary query={hubQuery} what="{remote.label} library">
				{#snippet loading()}
					<HubPageSkeleton />
				{/snippet}
				{#snippet children(hub)}
					{#if sessions.length > 0 || hub.recently_played.length > 0}
						<div class="hub-pair">
							<HubNowPlaying {sessions} sourceLabel={remote.label} />
							<FeaturedAlbumHero
								albums={hub.recently_played}
								onAlbumClick={openAlbum}
								onplay={continueAlbum}
							/>
						</div>
					{/if}

					{#if caps.similarAlbums}
						<HubSimilarAlbums
							{remote}
							seed={hub.recently_played[0] ?? favoritesQuery.data?.albums[0] ?? null}
							onopen={openAlbum}
						/>
					{/if}

					<HubDiscovery {remote} genres={hub.genres} onopen={openAlbum}>
						{#snippet lead()}
							{#if caps.favorites}
								<HubFavorites
									favorites={favoritesQuery.data}
									loading={favoritesQuery.isFetching}
									onopen={openAlbum}
								/>
							{/if}
						{/snippet}
					</HubDiscovery>

					{#if caps.recentlyAdded}
						<div use:reveal>
							<HubShelf
								title="Recently Added"
								icon={CircleDashed}
								seeAllHref={recentlyAddedHref}
								bare
							>
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
							<HubShelf title="Most Played" icon={Disc3}>
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
						<HubHistory {remote} />
					{/if}

					<HubShelf title="Artists A-Z" icon={LibraryBig} loading={artistIndexQuery.isFetching}>
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

					<HubShelf
						title="Browse Albums"
						icon={Disc3}
						seeAllHref={withBasePath(`${base}/albums`)}
						bare
					>
						{#if hub.all_albums_preview.length > 0}
							<AlbumCarousel albums={hub.all_albums_preview} onopen={openAlbum} />
						{:else}
							<p class="text-sm text-muted">No albums found.</p>
						{/if}
					</HubShelf>
				{/snippet}
			</QueryBoundary>
		{/if}
	</div>
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

<style>
	.hub :global(.hub__line) {
		margin: 0 2rem;
	}
	.hub__body {
		display: flex;
		flex-direction: column;
		gap: 3rem;
		padding: 0.5rem 2rem 3rem;
	}
	.hub__top {
		display: grid;
		gap: 1.25rem;
		container: hub-top / inline-size;
	}
	@media (min-width: 1280px) {
		.hub__top {
			grid-template-columns: minmax(0, 1.05fr) minmax(0, 1fr);
			align-items: stretch;
		}
	}
	.hub :global(.dn-section-head__icon) {
		color: var(--dn-title);
	}

	@media (max-width: 1023px) {
		.hub :global(.hub__line) {
			margin: 0 1.5rem;
		}
		.hub__body {
			padding-inline: 1.5rem;
		}
	}
	@media (max-width: 767px) {
		.hub :global(.hub__line) {
			margin: 0 1rem;
		}
		.hub__body {
			gap: 2.25rem;
			padding: 0.25rem 1rem 2rem;
		}
	}
</style>
