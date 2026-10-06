<script lang="ts">
	import { api, ApiError } from '$lib/api/client';
	import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
	import {
		getRemoteArtistIndexQuery,
		getRemoteFavoritesQuery,
		getRemoteHubQuery,
		getRemoteInfoArtistQuery,
		getRemoteRandomQuery,
		getRemoteSimilarQuery,
		getRemoteTopQuery
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
	import FeaturedAlbumHero from '$lib/components/FeaturedAlbumHero.svelte';
	import HubPageSkeleton from '$lib/components/HubPageSkeleton.svelte';
	import SourceAlbumModal from '$lib/components/SourceAlbumModal.svelte';
	import PlaylistImportBanner from '$lib/components/PlaylistImportBanner.svelte';
	import NowPlayingWidget from '$lib/components/NowPlayingWidget.svelte';
	import { nowPlayingMerged } from '$lib/stores/nowPlayingMerged.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import ArtistIndexSidebar from '$lib/components/ArtistIndexSidebar.svelte';
	import GenreSongsBrowser from '$lib/components/GenreSongsBrowser.svelte';
	import type { BrowseTrack } from '$lib/components/GenreSongsBrowser.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { buildDiscoveryQueueFromNavidrome } from '$lib/player/queueHelpers';
	import { formatDurationSec as formatDuration } from '$lib/utils/formatting';
	import { reveal } from '$lib/actions/reveal';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { getApiUrl } from '$lib/api/api-utils';
	import type {
		NavidromeAlbumSummary,
		NavidromeTrackInfo,
		ArtistIndexEntry,
		BrowseHeroCard
	} from '$lib/types';
	import type { DiscoveryTrack } from '$lib/components/DiscoveryTrackTable.svelte';

	const playlistsQuery = getSourcePlaylistsQuery(() => 'navidrome');
	const playlistCollection = $derived(playlistsQuery.data);
	const playlistErrorCode = $derived(
		playlistsQuery.error instanceof ApiError
			? playlistsQuery.error.code || 'SOURCE_PLAYLISTS_UNAVAILABLE'
			: playlistsQuery.isError
				? 'SOURCE_PLAYLISTS_UNAVAILABLE'
				: ''
	);

	let selectedAlbum = $state<NavidromeAlbumSummary | null>(null);
	let modalOpen = $state(false);
	let favTab = $state<'albums' | 'artists' | 'tracks'>('albums');
	let selectedGenre = $state<string | undefined>(undefined);
	let refreshing = $state(false);

	const hubQuery = getRemoteHubQuery(() => 'navidrome');
	const favoritesQuery = getRemoteFavoritesQuery(() => 'navidrome');
	const artistIndexQuery = getRemoteArtistIndexQuery(() => 'navidrome');
	const randomQuery = getRemoteRandomQuery(
		() => 'navidrome',
		() => ({
			limit: 20,
			genre: selectedGenre
		})
	);

	const hub = $derived<RemoteHub | null>(hubQuery.data ?? null);
	const loading = $derived(hubQuery.isPending);
	const error = $derived(hubQuery.isError ? "Couldn't connect to Navidrome." : '');

	const topSeedName = $derived(hub?.favorite_artists?.[0]?.name ?? '');
	const similarSeedId = $derived(favoritesQuery.data?.tracks?.[0]?.id ?? '');
	const artistSeed = $derived(hub?.favorite_artists?.[0] ?? null);

	const topQuery = getRemoteTopQuery(
		() => 'navidrome',
		() => topSeedName,
		() => 20
	);
	const similarQuery = getRemoteSimilarQuery(
		() => 'navidrome',
		() => similarSeedId,
		() => ({ limit: 20 })
	);
	const artistInfoQuery = getRemoteInfoArtistQuery(
		() => 'navidrome',
		() => artistSeed?.id ?? ''
	);

	const randomTracks = $derived<RemoteTrack[]>(randomQuery.data?.items ?? []);
	const randomLoading = $derived(randomQuery.isFetching);
	const topSongs = $derived<RemoteTrack[]>(topQuery.data?.items ?? []);
	const topSongsLoading = $derived(topQuery.isFetching);
	const topSongsArtist = $derived(topSeedName);
	const similarSongs = $derived<RemoteTrack[]>(similarQuery.data?.items ?? []);
	const similarLoading = $derived(similarQuery.isFetching);
	const artistInfo = $derived(artistInfoQuery.data ?? null);
	const artistInfoLoading = $derived(artistInfoQuery.isFetching);
	const artistSeedName = $derived(artistSeed?.name ?? '');

	const favoriteAlbums = $derived((favoritesQuery.data?.albums ?? []).map(toAlbumSummary));
	const favoriteArtists = $derived<RemoteArtist[]>(favoritesQuery.data?.artists ?? []);
	const favoriteTracks = $derived<RemoteTrack[]>(favoritesQuery.data?.tracks ?? []);
	const favoritesLoading = $derived(favoritesQuery.isFetching);
	const recentlyPlayed = $derived((hub?.recently_played ?? []).map(toAlbumSummary));
	const allAlbumsPreview = $derived((hub?.all_albums_preview ?? []).map(toAlbumSummary));

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

	const navidromeSessions = $derived(nowPlayingMerged.sessionsForSource('navidrome'));

	async function refreshHub() {
		refreshing = true;
		try {
			await Promise.all([
				playlistsQuery.refetch(),
				hubQuery.refetch(),
				favoritesQuery.refetch(),
				randomQuery.refetch(),
				topQuery.refetch(),
				similarQuery.refetch(),
				artistInfoQuery.refetch(),
				artistIndexQuery.refetch()
			]);
		} finally {
			refreshing = false;
		}
	}

	// Shared shelves and the album modal still take the per-source summary
	// shapes, so remote albums map at the page edge.
	function toAlbumSummary(album: RemoteAlbum): NavidromeAlbumSummary {
		return {
			navidrome_id: album.id,
			name: album.title,
			artist_name: album.artist_name,
			year: album.year ?? null,
			track_count: album.track_count ?? 0,
			image_url: album.image_url ?? null,
			musicbrainz_id: album.release_group_mbid ?? album.release_mbid ?? null,
			artist_musicbrainz_id: album.artist_mbid ?? null
		};
	}

	function toTrackInfo(track: RemoteTrack): NavidromeTrackInfo {
		return {
			navidrome_id: track.id,
			title: track.title,
			track_number: track.track_number ?? 0,
			disc_number: track.disc_number ?? null,
			duration_seconds: track.duration_secs ?? 0,
			album_name: track.album_name,
			artist_name: track.artist_name,
			image_url: track.image_url ?? null
		};
	}

	function toDiscoveryTracks(tracks: RemoteTrack[]): DiscoveryTrack[] {
		return tracks.map((t) => ({
			id: t.id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			duration_seconds: t.duration_secs ?? 0,
			image_url: t.image_url ?? undefined
		}));
	}

	async function fetchNavidromeGenreSongs(
		genres: string[],
		limit: number,
		offset: number
	): Promise<BrowseTrack[]> {
		if (genres.length === 0) return [];
		const page = await api.global.v3.GET(
			REMOTE_ENDPOINTS.genresSongs('navidrome', genres, { limit, offset })
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

	function buildNavidromeGenreQueue(tracks: BrowseTrack[]) {
		const navidromeTracks: NavidromeTrackInfo[] = tracks.map((t) => ({
			navidrome_id: t.id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			duration_seconds: t.duration_seconds,
			track_number: 0,
			image_url: t.image_url ?? null
		}));
		return buildDiscoveryQueueFromNavidrome(navidromeTracks);
	}

	function playRandomTracks(startIndex = 0) {
		if (randomTracks.length === 0) return;
		const items = buildDiscoveryQueueFromNavidrome(randomTracks.map(toTrackInfo));
		playerStore.playQueue(items, startIndex);
	}

	function playTopSongs(startIndex = 0) {
		if (topSongs.length === 0) return;
		const items = buildDiscoveryQueueFromNavidrome(topSongs.map(toTrackInfo));
		playerStore.playQueue(items, startIndex);
	}

	function playSimilarSongs(startIndex = 0) {
		if (similarSongs.length === 0) return;
		const items = buildDiscoveryQueueFromNavidrome(similarSongs.map(toTrackInfo));
		playerStore.playQueue(items, startIndex);
	}

	function openAlbumDetail(album: NavidromeAlbumSummary) {
		selectedAlbum = album;
		modalOpen = true;
	}

	let browseCards = $derived<BrowseHeroCard[]>([
		{
			label: 'Albums',
			value: hub?.stats?.total_albums ?? null,
			href: withBasePath('/library/navidrome/albums'),
			subtitle: 'in your library',
			colorScheme: 'primary',
			icon: 'disc'
		},
		{
			label: 'Artists',
			value: hub?.stats?.total_artists ?? null,
			href: withBasePath('/library/navidrome/artists'),
			subtitle: 'in your library',
			colorScheme: 'secondary',
			icon: 'users'
		},
		{
			label: 'Tracks',
			value: hub?.stats?.total_tracks ?? null,
			href: withBasePath('/library/navidrome/tracks'),
			subtitle: 'in your library',
			colorScheme: 'accent',
			icon: 'music'
		}
	]);
</script>

<div class="container mx-auto space-y-6 p-6">
	<div
		class="h-[2px] rounded-full bg-gradient-to-r from-transparent via-[rgb(var(--brand-navidrome))] to-transparent opacity-40"
	></div>

	<SourceHubHeader
		title="Navidrome Library"
		albumCount={hub?.stats?.total_albums ?? null}
		onrefresh={refreshHub}
		{refreshing}
	>
		{#snippet icon()}
			<span style="color: rgb(var(--brand-navidrome));">
				<NavidromeIcon class="h-8 w-8" />
			</span>
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
		sourceLabel="Navidrome"
		playlistsHref={withBasePath('/library/navidrome/playlists')}
	>
		{#snippet sourceIcon()}
			<span style="color: rgb(var(--brand-navidrome));">
				<NavidromeIcon class="h-4 w-4" />
			</span>
		{/snippet}
	</PlaylistImportBanner>

	<NowPlayingWidget sessions={navidromeSessions} />

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
			idKey="navidrome_id"
			onAlbumClick={(a) => openAlbumDetail(a as NavidromeAlbumSummary)}
		/>

		{#if favoriteAlbums.length > 0 || favoriteArtists.length > 0 || favoriteTracks.length > 0}
			<div use:reveal>
				<HubShelf title="Favorites" loading={favoritesLoading}>
					{#if favoriteAlbums.length > 0 || favoriteArtists.length > 0 || favoriteTracks.length > 0}
						<div role="tablist" class="tabs tabs-box tabs-sm mb-3">
							<button
								role="tab"
								class="tab"
								class:tab-active={favTab === 'albums'}
								onclick={() => (favTab = 'albums')}
							>
								Albums ({favoriteAlbums.length})
							</button>
							<button
								role="tab"
								class="tab"
								class:tab-active={favTab === 'artists'}
								onclick={() => (favTab = 'artists')}
							>
								Artists ({favoriteArtists.length})
							</button>
							<button
								role="tab"
								class="tab"
								class:tab-active={favTab === 'tracks'}
								onclick={() => (favTab = 'tracks')}
							>
								Tracks ({favoriteTracks.length})
							</button>
						</div>

						{#if favTab === 'albums'}
							{#if favoriteAlbums.length > 0}
								<HorizontalCarousel>
									{#each favoriteAlbums as album (album.navidrome_id)}
										<SourceAlbumCardCompact
											imageId={album.musicbrainz_id ?? album.navidrome_id}
											imageUrl={album.image_url}
											name={album.name}
											artistName={album.artist_name}
											onclick={() => openAlbumDetail(album)}
										/>
									{/each}
								</HorizontalCarousel>
							{:else}
								<p class="text-sm text-base-content/50">No favorite albums yet.</p>
							{/if}
						{:else if favTab === 'artists'}
							{#if favoriteArtists.length > 0}
								<HorizontalCarousel>
									{#each favoriteArtists as artist (artist.id)}
										<div class="shrink-0 w-32 text-center">
											<div class="mx-auto h-28 w-28 overflow-hidden rounded-full">
												<ArtistImage
													mbid={artist.artist_mbid ?? artist.id}
													remoteUrl={artist.image_url}
													alt={artist.name}
													size="full"
													requestSize={250}
													rounded="full"
													className="h-full w-full"
												/>
											</div>
											<p class="text-sm font-medium mt-1 line-clamp-1">{artist.name}</p>
											<p class="text-xs opacity-60">
												{artist.album_count ?? 0} album{(artist.album_count ?? 0) !== 1 ? 's' : ''}
											</p>
										</div>
									{/each}
								</HorizontalCarousel>
							{:else}
								<p class="text-sm text-base-content/50">No favorite artists yet.</p>
							{/if}
						{:else if favTab === 'tracks'}
							{#if favoriteTracks.length > 0}
								<div class="max-h-72 overflow-y-auto rounded-lg">
									<table class="table table-sm">
										<thead>
											<tr>
												<th>#</th>
												<th>Title</th>
												<th>Artist</th>
												<th>Album</th>
												<th class="text-right">Duration</th>
											</tr>
										</thead>
										<tbody>
											{#each favoriteTracks as track, i (track.id)}
												<tr
													class="hover transition-all duration-200 hover:border-l-2 hover:border-l-primary hover:pl-1"
												>
													<td class="text-base-content/50">{i + 1}</td>
													<td class="font-medium">{track.title}</td>
													<td class="text-base-content/60">{track.artist_name}</td>
													<td class="text-base-content/60">{track.album_name}</td>
													<td class="text-right text-base-content/50"
														>{formatDuration(track.duration_secs ?? 0)}</td
													>
												</tr>
											{/each}
										</tbody>
									</table>
								</div>
							{:else}
								<p class="text-sm text-base-content/50">No favorite tracks yet.</p>
							{/if}
						{/if}
					{/if}
				</HubShelf>
			</div>
		{/if}

		<DiscoveryZone>
			<DiscoveryShelf
				title="Surprise Me"
				loading={randomLoading}
				empty={!randomLoading && randomTracks.length === 0 && !loading}
				emptyMessage="Refresh to load a new batch of random tracks."
				onrefresh={() => void randomQuery.refetch()}
			>
				{#snippet actions()}
					{#if hub && hub.genres.length > 0}
						<GenrePillFilter
							genres={hub.genres.slice(0, 12)}
							selected={selectedGenre}
							loading={randomLoading}
							showAll={true}
							onselect={(g) => {
								selectedGenre = g;
							}}
						/>
					{/if}
				{/snippet}
				{#if randomTracks.length > 0}
					<div class="flex items-center gap-2 mb-3 mt-3">
						<button class="btn btn-primary btn-sm" onclick={() => playRandomTracks()}>
							Play all
						</button>
						<button
							class="btn btn-ghost btn-sm"
							onclick={() => {
								const items = buildDiscoveryQueueFromNavidrome(randomTracks.map(toTrackInfo));
								playerStore.playQueue(items, 0, true);
							}}
						>
							Shuffle
						</button>
					</div>
					<DiscoveryTrackTable
						tracks={toDiscoveryTracks(randomTracks)}
						onplay={(i) => playRandomTracks(i)}
						{formatDuration}
					/>
				{/if}
			</DiscoveryShelf>

			<div
				class="h-px bg-gradient-to-r from-transparent via-base-content/5 to-transparent my-4"
			></div>

			<HubShelf title="Browse by Genre" {loading}>
				{#if hub && hub.genres.length > 0}
					<GenreSongsBrowser
						genres={hub.genres}
						fetchSongs={fetchNavidromeGenreSongs}
						buildQueue={buildNavidromeGenreQueue}
						multiSelect
					/>
				{:else if hub}
					<p class="text-sm text-base-content/50">No genres found.</p>
				{/if}
			</HubShelf>
		</DiscoveryZone>

		{#if topSongsArtist}
			<div use:reveal>
				<DiscoveryShelf
					title="Top Songs for {topSongsArtist}"
					loading={topSongsLoading}
					empty={!topSongsLoading && topSongs.length === 0}
					emptyMessage="Connect Last.fm to see top songs."
					onrefresh={() => void topQuery.refetch()}
				>
					{#if topSongs.length > 0}
						<div class="flex items-center gap-2 mb-3">
							<button class="btn btn-primary btn-sm" onclick={() => playTopSongs()}>Play all</button
							>
						</div>
						<DiscoveryTrackTable
							tracks={toDiscoveryTracks(topSongs)}
							onplay={(i) => playTopSongs(i)}
							{formatDuration}
						/>
					{/if}
				</DiscoveryShelf>
			</div>
		{/if}

		{#if favoriteTracks.length > 0}
			<div use:reveal>
				<DiscoveryShelf
					title="Similar Songs"
					loading={similarLoading}
					empty={!similarLoading && similarSongs.length === 0}
					emptyMessage="Connect Last.fm to see similar songs."
					onrefresh={() => void similarQuery.refetch()}
				>
					{#if similarSongs.length > 0}
						<div class="flex items-center gap-2 mb-3">
							<button class="btn btn-primary btn-sm" onclick={() => playSimilarSongs()}
								>Play all</button
							>
						</div>
						<DiscoveryTrackTable
							tracks={toDiscoveryTracks(similarSongs)}
							onplay={(i) => playSimilarSongs(i)}
							{formatDuration}
						/>
					{/if}
				</DiscoveryShelf>
			</div>
		{/if}

		{#if artistInfo}
			<div use:reveal>
				<HubShelf title="About {artistSeedName}" loading={artistInfoLoading}>
					<div class="flex gap-4 items-start">
						{#if artistInfo.image_url}
							<img
								src={getApiUrl(artistInfo.image_url)}
								alt={artistSeedName}
								class="w-24 h-24 rounded-full object-cover shrink-0"
							/>
						{/if}
						<div class="space-y-2">
							{#if artistInfo.biography}
								<p class="text-sm text-base-content/70 line-clamp-4">{artistInfo.biography}</p>
							{/if}
							{#if artistInfo.similar_artists.length > 0}
								<div>
									<p class="text-xs font-semibold text-base-content/50 mb-1">Similar Artists</p>
									<div class="flex flex-wrap gap-1">
										{#each artistInfo.similar_artists.slice(0, 8) as sa (sa.id)}
											<span class="badge badge-sm badge-outline">{sa.name}</span>
										{/each}
									</div>
								</div>
							{/if}
						</div>
					</div>
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
										`/library/navidrome/albums?search=${encodeURIComponent(artist.name)}`
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
				seeAllHref={withBasePath('/library/navidrome/albums')}
				{loading}
			>
				{#if allAlbumsPreview.length > 0}
					<HorizontalCarousel>
						{#each allAlbumsPreview as album (album.navidrome_id)}
							<SourceAlbumCardCompact
								imageId={album.musicbrainz_id ?? album.navidrome_id}
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
	sourceType="navidrome"
	album={selectedAlbum}
	onclose={() => {
		modalOpen = false;
		selectedAlbum = null;
	}}
/>
