<script lang="ts">
	import { api, ApiError } from '$lib/api/client';
	import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
	import {
		getRemoteArtistIndexQuery,
		getRemoteFavoritesQuery,
		getRemoteHubQuery,
		getRemoteMixQuery,
		getRemoteSimilarQuery
	} from '$lib/queries/remotes/RemoteQueries.svelte';
	import type {
		RemoteAlbum,
		RemoteArtist,
		RemoteHub,
		RemoteTrack
	} from '$lib/queries/remotes/types';
	import { getSourcePlaylistsQuery } from '$lib/queries/source-playlists/SourcePlaylistQueries.svelte';
	import SourceAlbumCardCompact from '$lib/components/SourceAlbumCardCompact.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import HorizontalCarousel from '$lib/components/HorizontalCarousel.svelte';
	import SourceHubHeader from '$lib/components/SourceHubHeader.svelte';
	import BrowseHeroCards from '$lib/components/BrowseHeroCards.svelte';
	import HubShelf from '$lib/components/HubShelf.svelte';
	import DiscoveryZone from '$lib/components/DiscoveryZone.svelte';
	import DiscoveryShelf from '$lib/components/DiscoveryShelf.svelte';
	import DiscoveryTrackTable from '$lib/components/DiscoveryTrackTable.svelte';
	import GenrePillFilter from '$lib/components/GenrePillFilter.svelte';
	import GenreSongsBrowser from '$lib/components/GenreSongsBrowser.svelte';
	import type { BrowseTrack } from '$lib/components/GenreSongsBrowser.svelte';
	import FeaturedAlbumHero from '$lib/components/FeaturedAlbumHero.svelte';
	import HubPageSkeleton from '$lib/components/HubPageSkeleton.svelte';
	import AlbumGrid from '$lib/components/AlbumGrid.svelte';
	import MostPlayedSection from '$lib/components/MostPlayedSection.svelte';
	import SourceAlbumModal from '$lib/components/SourceAlbumModal.svelte';
	import PlaylistImportBanner from '$lib/components/PlaylistImportBanner.svelte';
	import NowPlayingWidget from '$lib/components/NowPlayingWidget.svelte';
	import ArtistIndexSidebar from '$lib/components/ArtistIndexSidebar.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { nowPlayingMerged } from '$lib/stores/nowPlayingMerged.svelte';
	import { buildDiscoveryQueueFromJellyfin } from '$lib/player/queueHelpers';
	import { formatDurationSec as formatDuration } from '$lib/utils/formatting';
	import { reveal } from '$lib/actions/reveal';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { Tv } from 'lucide-svelte';
	import type {
		JellyfinAlbumSummary,
		JellyfinTrackInfo,
		ArtistIndexEntry,
		BrowseHeroCard
	} from '$lib/types';
	import type { DiscoveryTrack } from '$lib/components/DiscoveryTrackTable.svelte';

	const playlistsQuery = getSourcePlaylistsQuery(() => 'jellyfin');
	const playlistCollection = $derived(playlistsQuery.data);
	const playlistErrorCode = $derived(
		playlistsQuery.error instanceof ApiError
			? playlistsQuery.error.code || 'SOURCE_PLAYLISTS_UNAVAILABLE'
			: playlistsQuery.isError
				? 'SOURCE_PLAYLISTS_UNAVAILABLE'
				: ''
	);

	let selectedAlbum = $state<JellyfinAlbumSummary | null>(null);
	let modalOpen = $state(false);
	let favTab = $state<'albums' | 'artists'>('albums');
	let mixLabel = $state('');
	let refreshing = $state(false);

	const hubQuery = getRemoteHubQuery(() => 'jellyfin');
	const favoritesQuery = getRemoteFavoritesQuery(() => 'jellyfin');
	const artistIndexQuery = getRemoteArtistIndexQuery(() => 'jellyfin');
	const mixQuery = getRemoteMixQuery(
		() => 'jellyfin',
		() => mixLabel,
		() => ({ kind: 'genre', limit: 30 })
	);

	const hub = $derived<RemoteHub | null>(hubQuery.data ?? null);
	const loading = $derived(hubQuery.isPending);
	const error = $derived(hubQuery.isError ? "Couldn't connect to Jellyfin." : '');

	const similarSeed = $derived(
		hub?.recently_played?.[0] ?? favoritesQuery.data?.albums?.[0] ?? null
	);
	const similarQuery = getRemoteSimilarQuery(
		() => 'jellyfin',
		() => similarSeed?.id ?? '',
		() => ({ limit: 30 })
	);

	const mixTracks = $derived<RemoteTrack[]>(mixQuery.data?.items ?? []);
	const mixLoading = $derived(mixQuery.isFetching);
	const similarSeedName = $derived(similarSeed?.title ?? '');
	// The v3 similar route returns tracks, so the shelf groups them back
	// into their albums to keep the album carousel.
	const similarAlbums = $derived(groupTracksByAlbum(similarQuery.data?.items ?? []));
	const similarLoading = $derived(similarQuery.isFetching);

	const favoriteAlbums = $derived((favoritesQuery.data?.albums ?? []).map(toAlbumSummary));
	const favoriteArtists = $derived<RemoteArtist[]>(favoritesQuery.data?.artists ?? []);
	const recentlyPlayed = $derived((hub?.recently_played ?? []).map(toAlbumSummary));
	const recentlyAdded = $derived((hub?.recently_added ?? []).map(toAlbumSummary));
	const allAlbumsPreview = $derived((hub?.all_albums_preview ?? []).map(toAlbumSummary));
	const mostPlayedArtists = $derived(hub?.most_played_artists ?? []);

	const genericArtistIndex = $derived<ArtistIndexEntry[]>(
		(artistIndexQuery.data?.index ?? []).map((e) => ({
			name: e.name,
			artists: e.artists.map((a) => ({
				id: a.id,
				name: a.name,
				image_url: a.image_url ?? null,
				album_count: a.album_count ?? undefined,
				musicbrainz_id: a.artist_mbid ?? null
			}))
		}))
	);
	const artistIndexLoading = $derived(artistIndexQuery.isFetching);

	const jellyfinSessions = $derived(nowPlayingMerged.sessionsForSource('jellyfin'));

	async function refreshHub() {
		refreshing = true;
		try {
			await Promise.all([
				playlistsQuery.refetch(),
				hubQuery.refetch(),
				favoritesQuery.refetch(),
				mixQuery.refetch(),
				similarQuery.refetch(),
				artistIndexQuery.refetch()
			]);
		} finally {
			refreshing = false;
		}
	}

	// Shared shelves and the album modal still take the per-source summary
	// shapes, so remote albums map at the page edge.
	function toAlbumSummary(album: RemoteAlbum): JellyfinAlbumSummary {
		return {
			jellyfin_id: album.id,
			name: album.title,
			artist_name: album.artist_name,
			year: album.year ?? null,
			track_count: album.track_count ?? 0,
			image_url: album.image_url ?? null,
			musicbrainz_id: album.release_group_mbid ?? album.release_mbid ?? null,
			artist_musicbrainz_id: album.artist_mbid ?? null
		};
	}

	function toTrackInfo(track: RemoteTrack): JellyfinTrackInfo {
		return {
			jellyfin_id: track.id,
			title: track.title,
			track_number: track.track_number ?? 0,
			disc_number: track.disc_number ?? null,
			duration_seconds: track.duration_secs ?? 0,
			album_name: track.album_name,
			artist_name: track.artist_name,
			album_id: track.album_id ?? undefined,
			image_url: track.image_url ?? null
		};
	}

	function toDiscoveryTracks(tracks: RemoteTrack[]): DiscoveryTrack[] {
		return tracks.map((t) => ({
			id: t.id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			album_id: t.album_id ?? undefined,
			image_url: t.image_url ?? undefined,
			duration_seconds: t.duration_secs ?? 0
		}));
	}

	function groupTracksByAlbum(tracks: RemoteTrack[]): JellyfinAlbumSummary[] {
		const albums: JellyfinAlbumSummary[] = [];
		const seen: string[] = [];
		for (const t of tracks) {
			const key = t.album_id ?? t.album_name;
			if (!key || seen.includes(key)) continue;
			seen.push(key);
			albums.push({
				jellyfin_id: t.album_id ?? key,
				name: t.album_name,
				artist_name: t.artist_name,
				track_count: 0,
				image_url: t.image_url ?? null,
				musicbrainz_id: null,
				year: t.year ?? null
			});
		}
		return albums;
	}

	function openAlbumDetail(album: JellyfinAlbumSummary) {
		selectedAlbum = album;
		modalOpen = true;
	}

	function playMixTracks(startIndex = 0) {
		if (mixTracks.length === 0) return;
		const items = buildDiscoveryQueueFromJellyfin(mixTracks.map(toTrackInfo));
		playerStore.playQueue(items, startIndex);
	}

	async function fetchJellyfinGenreSongs(
		genres: string[],
		limit: number,
		offset: number
	): Promise<BrowseTrack[]> {
		if (genres.length === 0) return [];
		const page = await api.global.v3.GET(
			REMOTE_ENDPOINTS.genresSongs('jellyfin', genres, { limit, offset })
		);
		return page.items.map((t) => ({
			id: t.id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			duration_seconds: t.duration_secs ?? 0,
			image_url: t.image_url ?? undefined
		}));
	}

	function buildJellyfinGenreQueue(tracks: BrowseTrack[]) {
		const jellyfinTracks: JellyfinTrackInfo[] = tracks.map((t) => ({
			jellyfin_id: t.id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			duration_seconds: t.duration_seconds,
			track_number: 0,
			image_url: t.image_url ?? null
		}));
		return buildDiscoveryQueueFromJellyfin(jellyfinTracks);
	}

	let browseCards = $derived<BrowseHeroCard[]>([
		{
			label: 'Albums',
			value: hub?.stats?.total_albums ?? null,
			href: withBasePath('/library/jellyfin/albums'),
			subtitle: 'in your library',
			colorScheme: 'primary',
			icon: 'disc'
		},
		{
			label: 'Artists',
			value: hub?.stats?.total_artists ?? null,
			href: withBasePath('/library/jellyfin/artists'),
			subtitle: 'in your library',
			colorScheme: 'secondary',
			icon: 'users'
		},
		{
			label: 'Tracks',
			value: hub?.stats?.total_tracks ?? null,
			href: withBasePath('/library/jellyfin/tracks'),
			subtitle: 'in your library',
			colorScheme: 'accent',
			icon: 'music'
		}
	]);
</script>

<div class="container mx-auto space-y-6 p-6">
	<div
		class="h-[2px] rounded-full bg-gradient-to-r from-transparent via-[rgb(var(--brand-jellyfin))] to-transparent opacity-40"
	></div>

	<SourceHubHeader
		title="Jellyfin Library"
		albumCount={hub?.stats?.total_albums ?? null}
		onrefresh={refreshHub}
		{refreshing}
	>
		{#snippet icon()}
			<Tv class="h-8 w-8 text-info" />
		{/snippet}
	</SourceHubHeader>

	<BrowseHeroCards cards={browseCards} />

	<PlaylistImportBanner
		playlists={playlistCollection?.playlists ?? []}
		accountMode={playlistCollection?.account_mode}
		accountLabel={playlistCollection?.account_label}
		loading={playlistsQuery.isPending}
		errorCode={playlistErrorCode}
		onretry={() => void playlistsQuery.refetch()}
		sourceLabel="Jellyfin"
		playlistsHref={withBasePath('/library/jellyfin/playlists')}
	>
		{#snippet sourceIcon()}
			<Tv class="h-4 w-4 text-info" />
		{/snippet}
	</PlaylistImportBanner>

	<NowPlayingWidget sessions={jellyfinSessions} />

	{#if error}
		<div role="alert" class="alert alert-error alert-soft">
			<span>{error}</span>
			<button class="btn btn-sm btn-ghost" onclick={() => location.reload()}>Retry</button>
		</div>
	{/if}

	{#if loading && !hub}
		<HubPageSkeleton />
	{:else}
		<FeaturedAlbumHero
			albums={recentlyPlayed}
			idKey="jellyfin_id"
			onAlbumClick={(a) => openAlbumDetail(a as JellyfinAlbumSummary)}
		/>

		{#if similarSeedName || similarLoading || similarAlbums.length > 0}
			<div use:reveal>
				<HubShelf
					title={similarSeedName ? `More Like "${similarSeedName}"` : 'More Like This'}
					loading={similarLoading}
				>
					{#if similarAlbums.length > 0}
						<HorizontalCarousel>
							{#each similarAlbums as album (album.jellyfin_id)}
								<SourceAlbumCardCompact
									imageId={album.musicbrainz_id ?? album.jellyfin_id}
									imageUrl={album.image_url}
									name={album.name}
									artistName={album.artist_name}
									onclick={() => openAlbumDetail(album)}
								/>
							{/each}
						</HorizontalCarousel>
					{:else if !similarLoading}
						<p class="text-sm text-base-content/50">No similar albums found.</p>
					{/if}
				</HubShelf>
			</div>
		{/if}

		{#if loading || favoriteAlbums.length > 0 || favoriteArtists.length > 0}
			<div use:reveal>
				<HubShelf title="Favorites" {loading}>
					<div class="flex gap-2 mb-3">
						{#if loading || favoriteAlbums.length > 0}
							<button
								class="badge cursor-pointer"
								class:badge-primary={favTab === 'albums'}
								class:badge-outline={favTab !== 'albums'}
								onclick={() => (favTab = 'albums')}>Albums</button
							>
						{/if}
						{#if loading || favoriteArtists.length > 0}
							<button
								class="badge cursor-pointer"
								class:badge-primary={favTab === 'artists'}
								class:badge-outline={favTab !== 'artists'}
								onclick={() => (favTab = 'artists')}>Artists</button
							>
						{/if}
					</div>
					{#if favTab === 'albums'}
						{#if favoriteAlbums.length > 0}
							<HorizontalCarousel>
								{#each favoriteAlbums as album (album.jellyfin_id)}
									<SourceAlbumCardCompact
										imageId={album.musicbrainz_id ?? album.jellyfin_id}
										imageUrl={album.image_url}
										name={album.name}
										artistName={album.artist_name}
										onclick={() => openAlbumDetail(album)}
									/>
								{/each}
							</HorizontalCarousel>
						{/if}
					{:else if favoriteArtists.length > 0}
						<HorizontalCarousel>
							{#each favoriteArtists as artist (artist.id)}
								<div class="shrink-0 w-28 text-center">
									<div class="w-24 h-24 mx-auto rounded-full overflow-hidden shadow-sm">
										<ArtistImage
											mbid={artist.artist_mbid ?? artist.id}
											remoteUrl={artist.image_url}
											alt={artist.name}
											size="full"
											requestSize={250}
										/>
									</div>
									<p class="text-sm font-medium mt-1 line-clamp-1">{artist.name}</p>
									<p class="text-xs opacity-60">
										{artist.album_count ?? 0} album{(artist.album_count ?? 0) !== 1 ? 's' : ''}
									</p>
								</div>
							{/each}
						</HorizontalCarousel>
					{/if}
				</HubShelf>
			</div>
		{/if}

		{#if (hub && hub.genres.length > 0) || mixTracks.length > 0}
			<DiscoveryZone>
				<DiscoveryShelf
					title="Instant Mix"
					loading={mixLoading}
					empty={!mixLoading && mixTracks.length === 0}
					emptyMessage="Pick a genre to build a mix."
					onrefresh={mixLabel ? () => void mixQuery.refetch() : undefined}
				>
					{#snippet actions()}
						{#if hub && hub.genres.length > 0}
							<GenrePillFilter
								genres={hub.genres.slice(0, 12)}
								selected={mixLabel || undefined}
								loading={mixLoading}
								onselect={(g) => {
									if (g) mixLabel = g;
								}}
							/>
						{/if}
					{/snippet}
					{#if mixTracks.length > 0}
						<div class="flex items-center gap-2 mb-3 mt-3">
							<button class="btn btn-primary btn-sm" onclick={() => playMixTracks()}
								>Play all</button
							>
							<button
								class="btn btn-ghost btn-sm"
								onclick={() => {
									const items = buildDiscoveryQueueFromJellyfin(mixTracks.map(toTrackInfo));
									playerStore.playQueue(items, 0, true);
								}}
							>
								Shuffle
							</button>
						</div>
						<DiscoveryTrackTable
							tracks={toDiscoveryTracks(mixTracks)}
							onplay={(i) => playMixTracks(i)}
							{formatDuration}
						/>
					{/if}
				</DiscoveryShelf>

				{#if hub && hub.genres.length > 0}
					<DiscoveryShelf title="Browse by Genre" emptyMessage="Pick a genre to browse tracks.">
						<GenreSongsBrowser
							genres={hub.genres}
							fetchSongs={fetchJellyfinGenreSongs}
							buildQueue={buildJellyfinGenreQueue}
							multiSelect
						/>
					</DiscoveryShelf>
				{/if}
			</DiscoveryZone>
		{/if}

		<div use:reveal>
			<HubShelf title="Recently Added" {loading}>
				{#if recentlyAdded.length > 0}
					<AlbumGrid
						albums={recentlyAdded}
						idKey="jellyfin_id"
						onAlbumClick={(a) => openAlbumDetail(a as JellyfinAlbumSummary)}
					/>
				{:else if hub}
					<p class="text-sm text-base-content/50">Nothing new yet.</p>
				{/if}
			</HubShelf>
		</div>

		{#if mostPlayedArtists.length > 0}
			<div use:reveal>
				<HubShelf title="Most Played" {loading}>
					<MostPlayedSection
						artists={mostPlayedArtists.map((a) => ({
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

		<div class="section-divider-glow"></div>

		<div class="mt-10 mb-6 rounded-2xl bg-base-200/20 p-6 space-y-6">
			<HubShelf title="Artists A-Z" loading={artistIndexLoading}>
				{#if genericArtistIndex.length > 0}
					<ArtistIndexSidebar
						index={genericArtistIndex}
						onselect={(artist) => {
							if (artist.musicbrainz_id) {
								goto(withBasePath(`/artist/${artist.musicbrainz_id}`));
							} else {
								goto(
									withBasePath(
										`/library/jellyfin/artists?search=${encodeURIComponent(artist.name)}`
									)
								);
							}
						}}
					/>
				{:else if !artistIndexLoading}
					<p class="text-sm text-base-content/50">No artists found.</p>
				{/if}
			</HubShelf>

			<HubShelf
				title="Browse Albums"
				seeAllHref={withBasePath('/library/jellyfin/albums')}
				{loading}
			>
				{#if allAlbumsPreview.length > 0}
					<HorizontalCarousel>
						{#each allAlbumsPreview as album (album.jellyfin_id)}
							<SourceAlbumCardCompact
								imageId={album.musicbrainz_id ?? album.jellyfin_id}
								imageUrl={album.image_url}
								name={album.name}
								artistName={album.artist_name}
								onclick={() => openAlbumDetail(album)}
							/>
						{/each}
					</HorizontalCarousel>
				{:else if hub}
					<p class="text-sm text-base-content/50">No albums found.</p>
				{/if}
			</HubShelf>
		</div>
	{/if}
</div>

<SourceAlbumModal
	bind:open={modalOpen}
	sourceType="jellyfin"
	album={selectedAlbum}
	onclose={() => {
		modalOpen = false;
		selectedAlbum = null;
	}}
/>
