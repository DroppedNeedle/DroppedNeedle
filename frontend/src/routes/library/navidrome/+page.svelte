<script lang="ts">
	import { resolve } from '$app/paths';
	import { API } from '$lib/constants';
	import { api, ApiError } from '$lib/api/client';
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
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import type {
		NavidromeHubResponse,
		NavidromeAlbumSummary,
		NavidromeTrackInfo,
		NavidromeArtistInfo,
		NavidromeArtistIndexResponse,
		NavidromeGenreSongsResponse,
		ArtistIndexEntry,
		BrowseHeroCard
	} from '$lib/types';
	import type { DiscoveryTrack } from '$lib/components/DiscoveryTrackTable.svelte';

	let hub = $state<NavidromeHubResponse | null>(null);
	let loading = $state(true);
	let error = $state('');
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

	let randomTracks = $state<NavidromeTrackInfo[]>([]);
	let randomLoading = $state(false);
	let selectedGenre = $state<string | undefined>(undefined);

	let topSongs = $state<NavidromeTrackInfo[]>([]);
	let topSongsLoading = $state(false);
	let topSongsArtist = $state('');

	let similarSongs = $state<NavidromeTrackInfo[]>([]);
	let similarLoading = $state(false);

	let artistInfo = $state<NavidromeArtistInfo | null>(null);
	let artistInfoLoading = $state(false);

	let artistIndex = $state<NavidromeArtistIndexResponse | null>(null);
	let artistIndexLoading = $state(false);
	let genericArtistIndex = $derived<ArtistIndexEntry[]>(
		artistIndex?.index.map((e) => ({
			name: e.name,
			artists: e.artists.map((a) => ({
				id: a.navidrome_id,
				name: a.name,
				image_url: a.image_url,
				album_count: a.album_count,
				musicbrainz_id: a.musicbrainz_id
			}))
		})) ?? []
	);

	let navidromeSessions = $derived(nowPlayingMerged.sessionsForSource('navidrome'));

	let refreshing = $state(false);

	async function refreshHub() {
		refreshing = true;
		try {
			await playlistsQuery.refetch();
			hub = await api.get<NavidromeHubResponse>(API.navidromeLibrary.hub());
			loadTopSongs();
			loadSimilarSongs();
			loadArtistInfo();
			loadArtistIndex();
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			return;
		} finally {
			refreshing = false;
		}
	}

	function toDiscoveryTracks(tracks: NavidromeTrackInfo[]): DiscoveryTrack[] {
		return tracks.map((t) => ({
			id: t.navidrome_id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			duration_seconds: t.duration_seconds,
			image_url: t.image_url ?? undefined
		}));
	}

	async function fetchNavidromeGenreSongs(
		genres: string[],
		limit: number,
		offset: number
	): Promise<BrowseTrack[]> {
		if (genres.length === 0) return [];
		let res: NavidromeGenreSongsResponse | null;
		if (genres.length === 1) {
			res = await api.get<NavidromeGenreSongsResponse>(
				API.navidromeLibrary.genreSongs(genres[0], limit, offset)
			);
		} else {
			res = await api.get<NavidromeGenreSongsResponse>(
				API.navidromeLibrary.multiGenreSongs(genres, limit, offset)
			);
		}
		if (!res) return [];
		return res.songs.map((t) => ({
			id: t.navidrome_id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			duration_seconds: t.duration_seconds,
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

	async function loadRandomTracks() {
		randomLoading = true;
		try {
			randomTracks = await api.get<NavidromeTrackInfo[]>(
				API.navidromeLibrary.random(20, selectedGenre)
			);
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			randomTracks = [];
		} finally {
			randomLoading = false;
		}
	}

	function playRandomTracks(startIndex = 0) {
		if (randomTracks.length === 0) return;
		const items = buildDiscoveryQueueFromNavidrome(randomTracks);
		playerStore.playQueue(items, startIndex);
	}

	async function loadTopSongs() {
		if (!hub) return;
		const artist = hub.favorite_artists?.[0];
		if (!artist) return;
		topSongsLoading = true;
		topSongsArtist = artist.name;
		try {
			topSongs = await api.get<NavidromeTrackInfo[]>(API.navidromeLibrary.topSongs(artist.name));
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			topSongs = [];
		} finally {
			topSongsLoading = false;
		}
	}

	async function loadSimilarSongs() {
		if (!hub) return;
		const recentTrack = hub.favorite_tracks?.[0];
		if (!recentTrack) return;
		similarLoading = true;
		try {
			similarSongs = await api.get<NavidromeTrackInfo[]>(
				API.navidromeLibrary.similarSongs(recentTrack.navidrome_id)
			);
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			similarSongs = [];
		} finally {
			similarLoading = false;
		}
	}

	async function loadArtistInfo() {
		if (!hub) return;
		const artist = hub.favorite_artists?.[0];
		if (!artist) return;
		artistInfoLoading = true;
		try {
			artistInfo = await api.get<NavidromeArtistInfo>(
				API.navidromeLibrary.artistInfo(artist.navidrome_id)
			);
			if (artistInfo) artistInfo.name = artist.name;
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			artistInfo = null;
		} finally {
			artistInfoLoading = false;
		}
	}

	function playTopSongs(startIndex = 0) {
		if (topSongs.length === 0) return;
		const items = buildDiscoveryQueueFromNavidrome(topSongs);
		playerStore.playQueue(items, startIndex);
	}

	function playSimilarSongs(startIndex = 0) {
		if (similarSongs.length === 0) return;
		const items = buildDiscoveryQueueFromNavidrome(similarSongs);
		playerStore.playQueue(items, startIndex);
	}

	function openAlbumDetail(album: NavidromeAlbumSummary) {
		selectedAlbum = album;
		modalOpen = true;
	}

	async function loadArtistIndex() {
		artistIndexLoading = true;
		try {
			artistIndex = await api.get<NavidromeArtistIndexResponse>(
				API.navidromeLibrary.artistsIndex()
			);
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			artistIndex = null;
		} finally {
			artistIndexLoading = false;
		}
	}

	let browseCards = $derived<BrowseHeroCard[]>([
		{
			label: 'Albums',
			value: hub?.stats?.total_albums ?? null,
			href: '/library/navidrome/albums',
			subtitle: 'in your library',
			colorScheme: 'primary',
			icon: 'disc'
		},
		{
			label: 'Artists',
			value: hub?.stats?.total_artists ?? null,
			href: '/library/navidrome/artists',
			subtitle: 'in your library',
			colorScheme: 'secondary',
			icon: 'users'
		},
		{
			label: 'Tracks',
			value: hub?.stats?.total_tracks ?? null,
			href: '/library/navidrome/tracks',
			subtitle: 'in your library',
			colorScheme: 'accent',
			icon: 'music'
		}
	]);

	onMount(() => {
		(async () => {
			try {
				hub = await api.get<NavidromeHubResponse>(API.navidromeLibrary.hub());
			} catch {
				error = "Couldn't connect to Navidrome.";
			} finally {
				loading = false;
			}
			loadTopSongs();
			loadSimilarSongs();
			loadArtistInfo();
			loadArtistIndex();
		})();
	});
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
		playlistsHref="/library/navidrome/playlists"
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
			albums={hub?.recently_played ?? []}
			idKey="navidrome_id"
			onAlbumClick={(a) => openAlbumDetail(a as NavidromeAlbumSummary)}
		/>

		{#if hub && (hub.favorites.length > 0 || hub.favorite_artists.length > 0 || hub.favorite_tracks.length > 0)}
			<div use:reveal>
				<HubShelf title="Favorites" {loading}>
					{#if hub && (hub.favorites.length > 0 || hub.favorite_artists.length > 0 || hub.favorite_tracks.length > 0)}
						<div role="tablist" class="tabs tabs-box tabs-sm mb-3">
							<button
								role="tab"
								class="tab"
								class:tab-active={favTab === 'albums'}
								onclick={() => (favTab = 'albums')}
							>
								Albums ({hub.favorites.length})
							</button>
							<button
								role="tab"
								class="tab"
								class:tab-active={favTab === 'artists'}
								onclick={() => (favTab = 'artists')}
							>
								Artists ({hub.favorite_artists.length})
							</button>
							<button
								role="tab"
								class="tab"
								class:tab-active={favTab === 'tracks'}
								onclick={() => (favTab = 'tracks')}
							>
								Tracks ({hub.favorite_tracks.length})
							</button>
						</div>

						{#if favTab === 'albums'}
							{#if hub.favorites.length > 0}
								<HorizontalCarousel>
									{#each hub.favorites as album (album.navidrome_id)}
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
							{#if hub.favorite_artists.length > 0}
								<HorizontalCarousel>
									{#each hub.favorite_artists as artist (artist.navidrome_id)}
										<div class="shrink-0 w-32 text-center">
											<div class="mx-auto h-28 w-28 overflow-hidden rounded-full">
												<ArtistImage
													mbid={artist.musicbrainz_id ?? artist.navidrome_id}
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
												{artist.album_count} album{artist.album_count !== 1 ? 's' : ''}
											</p>
										</div>
									{/each}
								</HorizontalCarousel>
							{:else}
								<p class="text-sm text-base-content/50">No favorite artists yet.</p>
							{/if}
						{:else if favTab === 'tracks'}
							{#if hub.favorite_tracks.length > 0}
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
											{#each hub.favorite_tracks as track, i (track.navidrome_id)}
												<tr
													class="hover transition-all duration-200 hover:border-l-2 hover:border-l-primary hover:pl-1"
												>
													<td class="text-base-content/50">{i + 1}</td>
													<td class="font-medium">{track.title}</td>
													<td class="text-base-content/60">{track.artist_name}</td>
													<td class="text-base-content/60">{track.album_name}</td>
													<td class="text-right text-base-content/50"
														>{formatDuration(track.duration_seconds)}</td
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
				onrefresh={loadRandomTracks}
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
								loadRandomTracks();
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
								const items = buildDiscoveryQueueFromNavidrome(randomTracks);
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
					onrefresh={loadTopSongs}
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

		{#if hub?.favorite_tracks?.length}
			<div use:reveal>
				<DiscoveryShelf
					title="Similar Songs"
					loading={similarLoading}
					empty={!similarLoading && similarSongs.length === 0}
					emptyMessage="Connect Last.fm to see similar songs."
					onrefresh={loadSimilarSongs}
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
				<HubShelf title="About {artistInfo.name}" loading={artistInfoLoading}>
					<div class="flex gap-4 items-start">
						{#if artistInfo.image_url}
							<img
								src={artistInfo.image_url}
								alt={artistInfo.name}
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
										{#each artistInfo.similar_artists.slice(0, 8) as sa (sa.navidrome_id || sa.name)}
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
								goto(resolve(`/artist/${artist.musicbrainz_id}`));
							} else {
								goto(
									resolve(`/library/navidrome/albums?search=${encodeURIComponent(artist.name)}`)
								);
							}
						}}
					/>
				{:else if !artistIndexLoading}
					<p class="text-sm text-base-content/50">No artists found.</p>
				{/if}
			</HubShelf>

			<HubShelf title="Browse Albums" seeAllHref="/library/navidrome/albums" {loading}>
				{#if hub && hub.all_albums_preview.length > 0}
					<HorizontalCarousel>
						{#each hub?.all_albums_preview ?? [] as album (album.navidrome_id)}
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
