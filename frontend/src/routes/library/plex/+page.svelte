<script lang="ts">
	import { API } from '$lib/constants';
	import { api, ApiError } from '$lib/api/client';
	import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
	import {
		getRemoteArtistIndexQuery,
		getRemoteDiscoveryQuery,
		getRemoteHistoryQuery,
		getRemoteHubQuery
	} from '$lib/queries/remotes/RemoteQueries.svelte';
	import type { RemoteAlbum, RemoteHub, RemoteTrack } from '$lib/queries/remotes/types';
	import { getSourcePlaylistsQuery } from '$lib/queries/source-playlists/SourcePlaylistQueries.svelte';
	import { resetPlexScrobblePreference } from '$lib/player/plexPlaybackApi';
	import SourceAlbumCardCompact from '$lib/components/SourceAlbumCardCompact.svelte';
	import HorizontalCarousel from '$lib/components/HorizontalCarousel.svelte';
	import SourceHubHeader from '$lib/components/SourceHubHeader.svelte';
	import BrowseHeroCards from '$lib/components/BrowseHeroCards.svelte';
	import HubShelf from '$lib/components/HubShelf.svelte';
	import DiscoveryZone from '$lib/components/DiscoveryZone.svelte';
	import DiscoveryShelf from '$lib/components/DiscoveryShelf.svelte';
	import GenreSongsBrowser from '$lib/components/GenreSongsBrowser.svelte';
	import type { BrowseTrack } from '$lib/components/GenreSongsBrowser.svelte';
	import FeaturedAlbumHero from '$lib/components/FeaturedAlbumHero.svelte';
	import HubPageSkeleton from '$lib/components/HubPageSkeleton.svelte';
	import AlbumGrid from '$lib/components/AlbumGrid.svelte';
	import SourceAlbumModal from '$lib/components/SourceAlbumModal.svelte';
	import PlaylistImportBanner from '$lib/components/PlaylistImportBanner.svelte';
	import NowPlayingWidget from '$lib/components/NowPlayingWidget.svelte';
	import ArtistIndexSidebar from '$lib/components/ArtistIndexSidebar.svelte';
	import { nowPlayingMerged } from '$lib/stores/nowPlayingMerged.svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import { toastStore } from '$lib/stores/toast';
	import { buildDiscoveryQueueFromPlex } from '$lib/player/queueHelpers';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import { reveal } from '$lib/actions/reveal';
	import { withBasePath } from '$lib/utils/basePath';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import type {
		PlexAlbumSummary,
		PlexConnectionSettings,
		PlexTrackInfo,
		ArtistIndexEntry,
		BrowseHeroCard
	} from '$lib/types';

	const playlistsQuery = getSourcePlaylistsQuery(() => 'plex');
	const playlistCollection = $derived(playlistsQuery.data);
	const playlistErrorCode = $derived(
		playlistsQuery.error instanceof ApiError
			? playlistsQuery.error.code || 'SOURCE_PLAYLISTS_UNAVAILABLE'
			: playlistsQuery.isError
				? 'SOURCE_PLAYLISTS_UNAVAILABLE'
				: ''
	);

	let scrobbleEnabled = $state(true);
	let scrobbleLoading = $state(false);

	let selectedAlbum = $state<PlexAlbumSummary | null>(null);
	let modalOpen = $state(false);
	let refreshing = $state(false);

	const hubQuery = getRemoteHubQuery(() => 'plex');
	const discoveryQuery = getRemoteDiscoveryQuery(
		() => 'plex',
		() => 10
	);
	const historyQuery = getRemoteHistoryQuery(
		() => 'plex',
		() => ({ limit: 10 })
	);
	const artistIndexQuery = getRemoteArtistIndexQuery(() => 'plex');

	const hub = $derived<RemoteHub | null>(hubQuery.data ?? null);
	const loading = $derived(hubQuery.isPending);
	const error = $derived(hubQuery.isError ? "Couldn't connect to Plex." : '');

	const discoveryHubs = $derived(
		(discoveryQuery.data?.hubs ?? []).map((dHub) => ({
			title: dHub.title,
			albums: dHub.albums.map(toAlbumSummary)
		}))
	);
	const discoveryLoading = $derived(discoveryQuery.isFetching);

	const historyEntries = $derived(historyQuery.data?.items ?? []);
	const historyTotal = $derived(historyQuery.data?.total ?? 0);
	const historyLoading = $derived(historyQuery.isFetching);

	const recentlyPlayed = $derived((hub?.recently_played ?? []).map(toAlbumSummary));
	const recentlyAdded = $derived((hub?.recently_added ?? []).map(toAlbumSummary));
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

	const plexSessions = $derived(nowPlayingMerged.sessionsForSource('plex'));

	async function refreshHub() {
		refreshing = true;
		try {
			await Promise.all([
				playlistsQuery.refetch(),
				hubQuery.refetch(),
				discoveryQuery.refetch(),
				historyQuery.refetch(),
				artistIndexQuery.refetch()
			]);
		} finally {
			refreshing = false;
		}
	}

	// Shared shelves and the album modal still take the per-source summary
	// shapes, so remote albums map at the page edge.
	function toAlbumSummary(album: RemoteAlbum): PlexAlbumSummary {
		return {
			plex_id: album.id,
			name: album.title,
			artist_name: album.artist_name,
			year: album.year ?? null,
			track_count: album.track_count ?? 0,
			image_url: album.image_url ?? null,
			musicbrainz_id: album.release_group_mbid ?? album.release_mbid ?? null,
			artist_musicbrainz_id: album.artist_mbid ?? null
		};
	}

	function toTrackInfo(track: RemoteTrack): PlexTrackInfo {
		return {
			plex_id: track.id,
			title: track.title,
			track_number: track.track_number ?? 0,
			duration_seconds: track.duration_secs ?? 0,
			disc_number: track.disc_number ?? 1,
			album_name: track.album_name,
			artist_name: track.artist_name,
			part_key: track.part_key ?? null,
			image_url: track.image_url ?? null
		};
	}

	function openAlbumDetail(album: PlexAlbumSummary) {
		selectedAlbum = album;
		modalOpen = true;
	}

	const plexGenreTrackMap = new SvelteMap<string, PlexTrackInfo>();
	const GENRE_MAP_MAX = 500;

	async function fetchPlexGenreSongs(
		genres: string[],
		limit: number,
		offset: number
	): Promise<BrowseTrack[]> {
		const genre = genres[0];
		if (!genre) return [];
		const page = await api.global.v3.GET(
			REMOTE_ENDPOINTS.genreSongs('plex', genre, { limit, offset })
		);
		if (plexGenreTrackMap.size > GENRE_MAP_MAX) plexGenreTrackMap.clear();
		for (const t of page.items) plexGenreTrackMap.set(t.id, toTrackInfo(t));
		return page.items.map((t) => ({
			id: t.id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			duration_seconds: t.duration_secs ?? 0,
			image_url: t.image_url ?? undefined
		}));
	}

	function buildPlexGenreQueue(tracks: BrowseTrack[]) {
		const plexTracks: PlexTrackInfo[] = tracks.map((t) => {
			const full = plexGenreTrackMap.get(t.id);
			return {
				plex_id: t.id,
				title: t.title,
				artist_name: t.artist_name,
				album_name: t.album_name,
				duration_seconds: t.duration_seconds,
				track_number: 0,
				disc_number: 1,
				part_key: full?.part_key ?? null,
				image_url: t.image_url ?? null
			};
		});
		return buildDiscoveryQueueFromPlex(plexTracks);
	}

	let browseCards = $derived<BrowseHeroCard[]>([
		{
			label: 'Albums',
			value: hub?.stats?.total_albums ?? null,
			href: withBasePath('/library/plex/albums'),
			subtitle: 'in your library',
			colorScheme: 'primary',
			icon: 'disc'
		},
		{
			label: 'Artists',
			value: hub?.stats?.total_artists ?? null,
			href: withBasePath('/library/plex/artists'),
			subtitle: 'in your library',
			colorScheme: 'secondary',
			icon: 'users'
		},
		{
			label: 'Tracks',
			value: hub?.stats?.total_tracks ?? null,
			href: withBasePath('/library/plex/tracks'),
			subtitle: 'in your library',
			colorScheme: 'accent',
			icon: 'music'
		}
	]);

	function formatViewedAt(viewedAt: number): string {
		const d = new Date(viewedAt * 1000);
		return d.toLocaleDateString(undefined, {
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit'
		});
	}

	// The scrobble toggle stays on the settings surface, which migrates
	// separately; only the library reads moved to remote queries.
	onMount(() => {
		(async () => {
			try {
				const settings = await api.get<PlexConnectionSettings>(API.settingsPlex());
				scrobbleEnabled = settings.scrobble_to_plex ?? false;
			} catch (err) {
				console.warn('[Hub] scrobble setting load failed:', err);
			}
		})();
	});

	async function toggleScrobble() {
		scrobbleLoading = true;
		try {
			const settings = await api.get<PlexConnectionSettings>(API.settingsPlex());
			settings.scrobble_to_plex = !scrobbleEnabled;
			await api.global.put(API.settingsPlex(), settings);
			scrobbleEnabled = settings.scrobble_to_plex;
			resetPlexScrobblePreference();
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			toastStore.show({
				message: "Couldn't update the Plex scrobble setting.",
				type: 'error'
			});
		} finally {
			scrobbleLoading = false;
		}
	}
</script>

<div class="container mx-auto space-y-6 p-6">
	<div
		class="h-[2px] rounded-full bg-gradient-to-r from-transparent via-[rgb(var(--brand-plex))] to-transparent opacity-40"
	></div>

	<SourceHubHeader
		title="Plex Library"
		albumCount={hub?.stats?.total_albums ?? null}
		onrefresh={refreshHub}
		{refreshing}
	>
		{#snippet icon()}
			<PlexIcon class="h-8 w-8" style="color: rgb(var(--brand-plex));" />
		{/snippet}
		{#snippet settingsSnippet()}
			<div class="tooltip tooltip-left" data-tip="Send play history back to Plex">
				<label class="label cursor-pointer gap-2 px-0">
					<span class="label-text text-sm opacity-70">Scrobble to Plex</span>
					<input
						type="checkbox"
						class="toggle toggle-sm"
						style="--tglbg: rgb(var(--brand-plex));"
						checked={scrobbleEnabled}
						disabled={scrobbleLoading}
						onchange={toggleScrobble}
					/>
				</label>
			</div>
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
		sourceLabel="Plex"
		playlistsHref={withBasePath('/library/plex/playlists')}
	>
		{#snippet sourceIcon()}
			<PlexIcon class="h-4 w-4" style="color: rgb(var(--brand-plex));" />
		{/snippet}
	</PlaylistImportBanner>

	<NowPlayingWidget sessions={plexSessions} />

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
			idKey="plex_id"
			onAlbumClick={(a) => openAlbumDetail(a as PlexAlbumSummary)}
		/>

		<DiscoveryZone>
			<DiscoveryShelf
				title="Recommended for you"
				loading={discoveryLoading}
				empty={!discoveryLoading && discoveryHubs.length === 0 && !loading}
				emptyMessage="No recommendations available right now."
				onrefresh={() => void discoveryQuery.refetch()}
			>
				{#each discoveryHubs as dHub (dHub.title)}
					<div class="mb-4">
						<h3 class="text-sm font-medium text-base-content/70 mb-2">{dHub.title}</h3>
						<HorizontalCarousel>
							{#each dHub.albums as album (album.plex_id)}
								<SourceAlbumCardCompact
									imageId={album.musicbrainz_id ?? album.plex_id}
									imageUrl={album.image_url}
									name={album.name}
									artistName={album.artist_name}
									onclick={() => openAlbumDetail(album)}
								/>
							{/each}
						</HorizontalCarousel>
					</div>
				{/each}
			</DiscoveryShelf>

			{#if hub && hub.genres.length > 0}
				<DiscoveryShelf title="Browse by Genre" emptyMessage="Pick a genre to browse tracks.">
					<GenreSongsBrowser
						genres={hub.genres}
						fetchSongs={fetchPlexGenreSongs}
						buildQueue={buildPlexGenreQueue}
					/>
				</DiscoveryShelf>
			{/if}
		</DiscoveryZone>

		<div use:reveal>
			<HubShelf
				title="Recently Added"
				{loading}
				seeAllHref={withBasePath('/library/plex/albums?sort=date_added')}
			>
				{#if recentlyAdded.length > 0}
					<AlbumGrid
						albums={recentlyAdded}
						idKey="plex_id"
						seeAllHref={withBasePath('/library/plex/albums?sort=date_added')}
						onAlbumClick={(a) => openAlbumDetail(a as PlexAlbumSummary)}
					/>
				{:else if hub}
					<p class="text-sm text-base-content/50">Nothing new yet.</p>
				{/if}
			</HubShelf>
		</div>

		<div class="section-divider-glow"></div>

		<div use:reveal>
			<HubShelf title="Listening History" loading={historyLoading}>
				{#if historyEntries.length > 0}
					<div class="overflow-x-auto rounded-lg">
						<table class="table table-sm">
							<thead>
								<tr>
									<th>Track</th>
									<th>Artist</th>
									<th>Album</th>
									<th>When</th>
								</tr>
							</thead>
							<tbody>
								{#each historyEntries as entry (entry.id + entry.viewed_at)}
									<tr
										class="hover transition-all duration-200 hover:border-l-2 hover:border-l-primary hover:pl-1"
									>
										<td class="font-medium">{entry.track_title}</td>
										<td class="text-base-content/60">{entry.artist_name}</td>
										<td class="text-base-content/60">{entry.album_name}</td>
										<td class="text-base-content/50 text-xs">{formatViewedAt(entry.viewed_at)}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
					{#if historyTotal > 10}
						<div class="mt-2 flex gap-2">
							<a href={withBasePath('/library/plex/activity')} class="btn btn-sm btn-outline">
								View full history and analytics ({historyTotal.toLocaleString()} plays)
							</a>
						</div>
					{/if}
				{:else if !historyLoading}
					<p class="text-sm text-base-content/50">No listening history is available yet.</p>
				{/if}
			</HubShelf>
		</div>

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
									withBasePath(`/library/plex/artists?search=${encodeURIComponent(artist.name)}`)
								);
							}
						}}
					/>
				{:else if !artistIndexLoading}
					<p class="text-sm text-base-content/50">No artists found.</p>
				{/if}
			</HubShelf>

			<HubShelf title="Browse Albums" seeAllHref={withBasePath('/library/plex/albums')} {loading}>
				{#if allAlbumsPreview.length > 0}
					<HorizontalCarousel>
						{#each allAlbumsPreview as album (album.plex_id)}
							<SourceAlbumCardCompact
								imageId={album.musicbrainz_id ?? album.plex_id}
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
	sourceType="plex"
	album={selectedAlbum}
	onclose={() => {
		modalOpen = false;
		selectedAlbum = null;
	}}
/>
