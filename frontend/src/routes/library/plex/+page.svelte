<script lang="ts">
	import { resolve } from '$app/paths';
	import { API } from '$lib/constants';
	import { api, ApiError } from '$lib/api/client';
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
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import type {
		PlexHubResponse,
		PlexAlbumSummary,
		PlexConnectionSettings,
		PlexDiscoveryResponse,
		PlexDiscoveryHub,
		PlexHistoryResponse,
		PlexHistoryEntry,
		PlexTrackInfo,
		PlexTrackPage,
		PlexArtistIndexResponse,
		ArtistIndexEntry,
		BrowseHeroCard
	} from '$lib/types';

	let hub = $state<PlexHubResponse | null>(null);
	let loading = $state(true);
	let error = $state('');
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

	let discoveryHubs = $state<PlexDiscoveryHub[]>([]);
	let discoveryLoading = $state(false);

	let historyEntries = $state<PlexHistoryEntry[]>([]);
	let historyTotal = $state(0);
	let historyLoading = $state(false);

	let artistIndex = $state<PlexArtistIndexResponse | null>(null);
	let artistIndexLoading = $state(false);
	let genericArtistIndex = $derived<ArtistIndexEntry[]>(
		artistIndex?.index.map((e) => ({
			name: e.name,
			artists: e.artists.map((a) => ({
				id: a.plex_id,
				name: a.name,
				image_url: a.image_url,
				musicbrainz_id: a.musicbrainz_id
			}))
		})) ?? []
	);

	let plexSessions = $derived(nowPlayingMerged.sessionsForSource('plex'));

	let refreshing = $state(false);

	async function refreshHub() {
		refreshing = true;
		try {
			await playlistsQuery.refetch();
			hub = await api.get<PlexHubResponse>(API.plexLibrary.hub());
			loadDiscovery();
			loadHistory();
			loadArtistIndex();
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			return;
		} finally {
			refreshing = false;
		}
	}

	async function loadDiscovery() {
		discoveryLoading = true;
		try {
			const resp = await api.get<PlexDiscoveryResponse>(API.plexLibrary.discovery());
			discoveryHubs = resp.hubs;
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			discoveryHubs = [];
		} finally {
			discoveryLoading = false;
		}
	}

	async function loadHistory() {
		historyLoading = true;
		try {
			const resp = await api.get<PlexHistoryResponse>(API.plexLibrary.history(10));
			historyEntries = resp.entries;
			historyTotal = resp.total;
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			historyEntries = [];
		} finally {
			historyLoading = false;
		}
	}

	function openAlbumDetail(album: PlexAlbumSummary) {
		selectedAlbum = album;
		modalOpen = true;
	}

	let plexGenreTrackMap = new SvelteMap<string, PlexTrackInfo>();
	const GENRE_MAP_MAX = 500;

	async function fetchPlexGenreSongs(
		genres: string[],
		limit: number,
		offset: number
	): Promise<BrowseTrack[]> {
		const genre = genres[0];
		if (!genre) return [];
		const res = await api.get<PlexTrackPage>(API.plexLibrary.genreSongs(genre, limit, offset));
		if (!res) return [];
		if (plexGenreTrackMap.size > GENRE_MAP_MAX) plexGenreTrackMap.clear();
		for (const t of res.items) plexGenreTrackMap.set(t.plex_id, t);
		return res.items.map((t) => ({
			id: t.plex_id,
			title: t.title,
			artist_name: t.artist_name,
			album_name: t.album_name,
			duration_seconds: t.duration_seconds,
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
			href: '/library/plex/albums',
			subtitle: 'in your library',
			colorScheme: 'primary',
			icon: 'disc'
		},
		{
			label: 'Artists',
			value: hub?.stats?.total_artists ?? null,
			href: '/library/plex/artists',
			subtitle: 'in your library',
			colorScheme: 'secondary',
			icon: 'users'
		},
		{
			label: 'Tracks',
			value: hub?.stats?.total_tracks ?? null,
			href: '/library/plex/tracks',
			subtitle: 'in your library',
			colorScheme: 'accent',
			icon: 'music'
		}
	]);

	function formatViewedAt(ts: string): string {
		try {
			const d = new Date(Number(ts) * 1000);
			return d.toLocaleDateString(undefined, {
				month: 'short',
				day: 'numeric',
				hour: '2-digit',
				minute: '2-digit'
			});
		} catch (err) {
			console.warn('[Hub] secondary load failed:', err);
			return '';
		}
	}

	onMount(() => {
		(async () => {
			try {
				const [hubData, settings] = await Promise.allSettled([
					api.get<PlexHubResponse>(API.plexLibrary.hub()),
					api.get<PlexConnectionSettings>(API.settingsPlex())
				]);
				if (hubData.status === 'fulfilled') {
					hub = hubData.value;
				} else {
					error = "Couldn't load Plex library data.";
				}
				if (settings.status === 'fulfilled') {
					scrobbleEnabled = settings.value.scrobble_to_plex ?? false;
				}
			} catch {
				error = "Couldn't connect to Plex.";
			} finally {
				loading = false;
			}
			loadDiscovery();
			loadHistory();
			loadArtistIndex();
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
	async function loadArtistIndex() {
		artistIndexLoading = true;
		try {
			artistIndex = await api.get<PlexArtistIndexResponse>(API.plexLibrary.artistsIndex());
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
		playlistsHref="/library/plex/playlists"
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
			albums={hub?.recently_played ?? []}
			idKey="plex_id"
			onAlbumClick={(a) => openAlbumDetail(a as PlexAlbumSummary)}
		/>

		<DiscoveryZone>
			<DiscoveryShelf
				title="Recommended for you"
				loading={discoveryLoading}
				empty={!discoveryLoading && discoveryHubs.length === 0 && !loading}
				emptyMessage="No recommendations available right now."
				onrefresh={loadDiscovery}
			>
				{#each discoveryHubs as dHub (dHub.title)}
					<div class="mb-4">
						<h3 class="text-sm font-medium text-base-content/70 mb-2">{dHub.title}</h3>
						<HorizontalCarousel>
							{#each dHub.albums as album (album.plex_id)}
								<SourceAlbumCardCompact
									imageId={album.plex_id}
									imageUrl={album.image_url}
									name={album.name}
									artistName={album.artist_name}
									onclick={() =>
										openAlbumDetail({
											plex_id: album.plex_id,
											name: album.name,
											artist_name: album.artist_name,
											year: album.year,
											image_url: album.image_url,
											track_count: 0
										})}
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
			<HubShelf title="Recently Added" {loading} seeAllHref="/library/plex/albums?sort=date_added">
				{#if hub && hub.recently_added.length > 0}
					<AlbumGrid
						albums={hub.recently_added}
						idKey="plex_id"
						seeAllHref="/library/plex/albums?sort=date_added"
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
									<th>Device</th>
								</tr>
							</thead>
							<tbody>
								{#each historyEntries as entry (entry.rating_key + entry.viewed_at)}
									<tr
										class="hover transition-all duration-200 hover:border-l-2 hover:border-l-primary hover:pl-1"
									>
										<td class="font-medium">{entry.track_title}</td>
										<td class="text-base-content/60">{entry.artist_name}</td>
										<td class="text-base-content/60">{entry.album_name}</td>
										<td class="text-base-content/50 text-xs">{formatViewedAt(entry.viewed_at)}</td>
										<td class="text-base-content/50 text-xs">{entry.device_name}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
					{#if historyTotal > 10}
						<div class="mt-2 flex gap-2">
							<a href={resolve('/library/plex/activity')} class="btn btn-sm btn-outline">
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
								goto(resolve(`/artist/${artist.musicbrainz_id}`));
							} else {
								goto(resolve(`/library/plex/artists?search=${encodeURIComponent(artist.name)}`));
							}
						}}
					/>
				{:else if !artistIndexLoading}
					<p class="text-sm text-base-content/50">No artists found.</p>
				{/if}
			</HubShelf>

			<HubShelf title="Browse Albums" seeAllHref="/library/plex/albums" {loading}>
				{#if hub && hub.all_albums_preview.length > 0}
					<HorizontalCarousel>
						{#each hub?.all_albums_preview ?? [] as album (album.plex_id)}
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
