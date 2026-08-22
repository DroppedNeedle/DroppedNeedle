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
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { Tv } from 'lucide-svelte';
	import type {
		JellyfinHubResponse,
		JellyfinAlbumSummary,
		JellyfinTrackInfo,
		JellyfinTrackPage,
		JellyfinFavoritesExpanded,
		JellyfinArtistIndexResponse,
		ArtistIndexEntry,
		BrowseHeroCard
	} from '$lib/types';
	import type { DiscoveryTrack } from '$lib/components/DiscoveryTrackTable.svelte';

	let hub = $state<JellyfinHubResponse | null>(null);
	let loading = $state(true);
	let error = $state('');
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

	let favExpanded = $state<JellyfinFavoritesExpanded | null>(null);
	let favTab = $state<'albums' | 'artists'>('albums');

	let mixTracks = $state<JellyfinTrackInfo[]>([]);
	let mixLoading = $state(false);
	let mixLabel = $state('');

	let similarAlbums = $state<JellyfinAlbumSummary[]>([]);
	let similarLoading = $state(false);
	let similarSeedName = $state('');

	let artistIndex = $state<JellyfinArtistIndexResponse | null>(null);
	let artistIndexLoading = $state(false);
	let genericArtistIndex = $derived<ArtistIndexEntry[]>(
		artistIndex?.index.map((e) => ({
			name: e.name,
			artists: e.artists.map((a) => ({
				id: a.jellyfin_id,
				name: a.name,
				image_url: a.image_url,
				album_count: a.album_count,
				musicbrainz_id: a.musicbrainz_id
			}))
		})) ?? []
	);

	let jellyfinSessions = $derived(nowPlayingMerged.sessionsForSource('jellyfin'));

	let refreshing = $state(false);

	async function refreshHub() {
		refreshing = true;
		try {
			await playlistsQuery.refetch();
			hub = await api.get<JellyfinHubResponse>(API.jellyfinLibrary.hub());
			loadSimilarAlbums();
			loadFavoritesExpanded();
			loadArtistIndex();
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			return;
		} finally {
			refreshing = false;
		}
	}

	function toDiscoveryTracks(tracks: JellyfinTrackInfo[]): DiscoveryTrack[] {
		return tracks.map((t) => ({
			id: t.jellyfin_id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			album_id: t.album_id,
			image_url: t.album_id ? `/api/v1/jellyfin/image/${t.album_id}` : null,
			duration_seconds: t.duration_seconds
		}));
	}

	function openAlbumDetail(album: JellyfinAlbumSummary) {
		selectedAlbum = album;
		modalOpen = true;
	}

	async function loadInstantMixByGenre(genre: string) {
		mixLoading = true;
		mixLabel = genre;
		try {
			mixTracks = await api.get<JellyfinTrackInfo[]>(
				API.jellyfinLibrary.instantMixByGenre(genre, 30)
			);
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			mixTracks = [];
		} finally {
			mixLoading = false;
		}
	}

	function playMixTracks(startIndex = 0) {
		if (mixTracks.length === 0) return;
		const items = buildDiscoveryQueueFromJellyfin(mixTracks);
		playerStore.playQueue(items, startIndex);
	}

	async function fetchJellyfinGenreSongs(
		genres: string[],
		limit: number,
		offset: number
	): Promise<BrowseTrack[]> {
		if (genres.length === 0) return [];
		const res = await api.get<JellyfinTrackPage>(
			API.jellyfinLibrary.genreSongs(genres, limit, offset)
		);
		if (!res) return [];
		return res.items.map((t) => ({
			id: t.jellyfin_id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			duration_seconds: t.duration_seconds,
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

	async function loadSimilarAlbums() {
		if (!hub) return;
		const seed = hub.recently_played?.[0] ?? hub.favorites?.[0] ?? hub.most_played_albums?.[0];
		if (!seed) return;
		similarSeedName = seed.name;
		similarLoading = true;
		try {
			similarAlbums = await api.get<JellyfinAlbumSummary[]>(
				API.jellyfinLibrary.similar(seed.jellyfin_id)
			);
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			similarAlbums = [];
		} finally {
			similarLoading = false;
		}
	}

	let browseCards = $derived<BrowseHeroCard[]>([
		{
			label: 'Albums',
			value: hub?.stats?.total_albums ?? null,
			href: '/library/jellyfin/albums',
			subtitle: 'in your library',
			colorScheme: 'primary',
			icon: 'disc'
		},
		{
			label: 'Artists',
			value: hub?.stats?.total_artists ?? null,
			href: '/library/jellyfin/artists',
			subtitle: 'in your library',
			colorScheme: 'secondary',
			icon: 'users'
		},
		{
			label: 'Tracks',
			value: hub?.stats?.total_tracks ?? null,
			href: '/library/jellyfin/tracks',
			subtitle: 'in your library',
			colorScheme: 'accent',
			icon: 'music'
		}
	]);

	onMount(() => {
		(async () => {
			try {
				hub = await api.get<JellyfinHubResponse>(API.jellyfinLibrary.hub());
			} catch {
				error = "Couldn't connect to Jellyfin.";
			} finally {
				loading = false;
			}
			loadSimilarAlbums();
			loadFavoritesExpanded();
			loadArtistIndex();
		})();
	});

	async function loadFavoritesExpanded() {
		try {
			favExpanded = await api.get<JellyfinFavoritesExpanded>(
				API.jellyfinLibrary.favoritesExpanded()
			);
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			favExpanded = null;
		}
	}

	async function loadArtistIndex() {
		artistIndexLoading = true;
		try {
			artistIndex = await api.get<JellyfinArtistIndexResponse>(API.jellyfinLibrary.artistsIndex());
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			artistIndex = null;
		} finally {
			artistIndexLoading = false;
		}
	}
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
		playlistsHref="/library/jellyfin/playlists"
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
			albums={hub?.recently_played ?? []}
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

		{#if loading || (favExpanded && (favExpanded.albums.length > 0 || favExpanded.artists.length > 0))}
			<div use:reveal>
				<HubShelf title="Favorites" {loading}>
					<div class="flex gap-2 mb-3">
						{#if !favExpanded || favExpanded.albums.length > 0}
							<button
								class="badge cursor-pointer"
								class:badge-primary={favTab === 'albums'}
								class:badge-outline={favTab !== 'albums'}
								onclick={() => (favTab = 'albums')}>Albums</button
							>
						{/if}
						{#if !favExpanded || favExpanded.artists.length > 0}
							<button
								class="badge cursor-pointer"
								class:badge-primary={favTab === 'artists'}
								class:badge-outline={favTab !== 'artists'}
								onclick={() => (favTab = 'artists')}>Artists</button
							>
						{/if}
					</div>
					{#if favTab === 'albums'}
						{#if favExpanded && favExpanded.albums.length > 0}
							<HorizontalCarousel>
								{#each favExpanded.albums as album (album.jellyfin_id)}
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
					{:else if favExpanded && favExpanded.artists.length > 0}
						<HorizontalCarousel>
							{#each favExpanded.artists as artist (artist.jellyfin_id)}
								<div class="shrink-0 w-28 text-center">
									<div class="w-24 h-24 mx-auto rounded-full overflow-hidden shadow-sm">
										<ArtistImage
											mbid={artist.musicbrainz_id ?? artist.jellyfin_id}
											remoteUrl={artist.image_url}
											alt={artist.name}
											size="full"
											requestSize={250}
										/>
									</div>
									<p class="text-sm font-medium mt-1 line-clamp-1">{artist.name}</p>
									<p class="text-xs opacity-60">
										{artist.album_count} album{artist.album_count !== 1 ? 's' : ''}
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
					onrefresh={mixLabel ? () => loadInstantMixByGenre(mixLabel) : undefined}
				>
					{#snippet actions()}
						{#if hub && hub.genres.length > 0}
							<GenrePillFilter
								genres={hub.genres.slice(0, 12)}
								selected={mixLabel || undefined}
								loading={mixLoading}
								onselect={(g) => {
									if (g) loadInstantMixByGenre(g);
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
									const items = buildDiscoveryQueueFromJellyfin(mixTracks);
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
				{#if hub && hub.recently_added.length > 0}
					<AlbumGrid
						albums={hub.recently_added}
						idKey="jellyfin_id"
						onAlbumClick={(a) => openAlbumDetail(a as JellyfinAlbumSummary)}
					/>
				{:else if hub}
					<p class="text-sm text-base-content/50">Nothing new yet.</p>
				{/if}
			</HubShelf>
		</div>

		{#if hub && (hub.most_played_artists.length > 0 || hub.most_played_albums.length > 0)}
			<div use:reveal>
				<HubShelf title="Most Played" {loading}>
					<MostPlayedSection
						artists={hub.most_played_artists.map((a) => ({
							id: a.jellyfin_id,
							name: a.name,
							image_url: a.image_url,
							musicbrainz_id: a.musicbrainz_id,
							play_count: a.play_count,
							album_count: a.album_count
						}))}
						albums={hub.most_played_albums.map((a) => ({
							id: a.jellyfin_id,
							name: a.name,
							artist_name: a.artist_name,
							image_url: a.image_url,
							musicbrainz_id: a.musicbrainz_id,
							play_count: a.play_count
						}))}
						onAlbumClick={(album) => {
							const orig = hub?.most_played_albums.find((a) => a.jellyfin_id === album.id);
							if (orig) openAlbumDetail(orig);
						}}
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
								goto(resolve(`/artist/${artist.musicbrainz_id}`));
							} else {
								goto(
									resolve(`/library/jellyfin/artists?search=${encodeURIComponent(artist.name)}`)
								);
							}
						}}
					/>
				{:else if !artistIndexLoading}
					<p class="text-sm text-base-content/50">No artists found.</p>
				{/if}
			</HubShelf>

			<HubShelf title="Browse Albums" seeAllHref="/library/jellyfin/albums" {loading}>
				{#if hub && hub.all_albums_preview.length > 0}
					<HorizontalCarousel>
						{#each hub?.all_albums_preview ?? [] as album (album.jellyfin_id)}
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
