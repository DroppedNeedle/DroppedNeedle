<script lang="ts">
	import type {
		AlbumBasicInfo,
		AlbumTracksInfo,
		YouTubeTrackLink,
		YouTubeLink,
		YouTubeQuotaStatus,
		JellyfinAlbumMatch,
		LocalAlbumMatch,
		NavidromeAlbumMatch,
		PlexAlbumMatch
	} from '$lib/types';
	import type { SourceCallbacks } from './albumPageState.svelte';
	import AlbumYouTubeBar from '$lib/components/AlbumYouTubeBar.svelte';
	import AlbumSourceBar from '$lib/components/AlbumSourceBar.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import LocalFilesIcon from '$lib/components/LocalFilesIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';

	interface Props {
		album: AlbumBasicInfo;
		tracksInfo: AlbumTracksInfo;
		trackLinks: YouTubeTrackLink[];
		albumLink: YouTubeLink | null;
		jellyfinMatch: JellyfinAlbumMatch | null;
		localMatch: LocalAlbumMatch | null;
		navidromeMatch: NavidromeAlbumMatch | null;
		plexMatch: PlexAlbumMatch | null;
		loadingJellyfin: boolean;
		loadingLocal: boolean;
		loadingNavidrome: boolean;
		loadingPlex: boolean;
		youtubeEnabled: boolean;
		youtubeApiConfigured: boolean;
		jellyfinEnabled: boolean;
		localfilesEnabled: boolean;
		navidromeEnabled: boolean;
		plexEnabled: boolean;
		jellyfinCallbacks: SourceCallbacks;
		localCallbacks: SourceCallbacks;
		localDownloadCallback?: { callback: (() => void) | undefined };
		navidromeCallbacks: SourceCallbacks;
		plexCallbacks: SourceCallbacks;
		onTrackLinksUpdate: (links: YouTubeTrackLink[]) => void;
		onAlbumLinkUpdate: (link: YouTubeLink) => void;
		onQuotaUpdate: (q: YouTubeQuotaStatus) => void;
	}

	let {
		album,
		tracksInfo,
		trackLinks,
		albumLink,
		jellyfinMatch,
		localMatch,
		navidromeMatch,
		plexMatch,
		loadingJellyfin,
		loadingLocal,
		loadingNavidrome,
		loadingPlex,
		youtubeEnabled,
		youtubeApiConfigured,
		jellyfinEnabled,
		localfilesEnabled,
		navidromeEnabled,
		plexEnabled,
		jellyfinCallbacks,
		localCallbacks,
		localDownloadCallback,
		navidromeCallbacks,
		plexCallbacks,
		onTrackLinksUpdate,
		onAlbumLinkUpdate,
		onQuotaUpdate
	}: Props = $props();
</script>

{#if youtubeEnabled}
	<AlbumYouTubeBar
		albumId={album.musicbrainz_id}
		albumName={album.title}
		artistName={album.artist_name}
		artistId={album.artist_id}
		coverUrl={album.cover_url ?? null}
		tracks={tracksInfo.tracks}
		{trackLinks}
		{albumLink}
		apiConfigured={youtubeApiConfigured}
		{onTrackLinksUpdate}
		{onAlbumLinkUpdate}
		{onQuotaUpdate}
	/>
{/if}

{#if jellyfinEnabled}
	{#if loadingJellyfin}
		<div class="skeleton h-14 w-full rounded-2xl"></div>
	{:else if jellyfinMatch?.found}
		<AlbumSourceBar
			sourceLabel="Jellyfin"
			sourceColor="rgb(var(--brand-jellyfin))"
			trackCount={jellyfinMatch.tracks.length}
			totalTracks={tracksInfo.tracks.length}
			onPlayAll={jellyfinCallbacks.onPlayAll}
			onShuffle={jellyfinCallbacks.onShuffle}
			onAddAllToQueue={jellyfinCallbacks.onAddAllToQueue}
			onPlayAllNext={jellyfinCallbacks.onPlayAllNext}
			onAddAllToPlaylist={jellyfinCallbacks.onAddAllToPlaylist}
		>
			{#snippet icon()}
				<JellyfinIcon class="h-5 w-5" />
			{/snippet}
		</AlbumSourceBar>
	{/if}
{/if}

{#if localfilesEnabled}
	{#if loadingLocal}
		<div class="skeleton h-14 w-full rounded-2xl"></div>
	{:else if localMatch?.found}
		<AlbumSourceBar
			sourceLabel="Local Files"
			sourceColor="rgb(var(--brand-localfiles))"
			trackCount={localMatch.tracks.length}
			totalTracks={tracksInfo.tracks.length}
			extraBadge={localMatch.primary_format?.toUpperCase() ?? null}
			onPlayAll={localCallbacks.onPlayAll}
			onShuffle={localCallbacks.onShuffle}
			onAddAllToQueue={localCallbacks.onAddAllToQueue}
			onPlayAllNext={localCallbacks.onPlayAllNext}
			onAddAllToPlaylist={localCallbacks.onAddAllToPlaylist}
			onDownload={localDownloadCallback?.callback}
		>
			{#snippet icon()}
				<LocalFilesIcon class="h-5 w-5" />
			{/snippet}
		</AlbumSourceBar>
	{/if}
{/if}

{#if navidromeEnabled}
	{#if loadingNavidrome}
		<div class="skeleton h-14 w-full rounded-2xl"></div>
	{:else if navidromeMatch?.found}
		<AlbumSourceBar
			sourceLabel="Navidrome"
			sourceColor="rgb(var(--brand-navidrome))"
			trackCount={navidromeMatch.tracks.length}
			totalTracks={tracksInfo.tracks.length}
			onPlayAll={navidromeCallbacks.onPlayAll}
			onShuffle={navidromeCallbacks.onShuffle}
			onAddAllToQueue={navidromeCallbacks.onAddAllToQueue}
			onPlayAllNext={navidromeCallbacks.onPlayAllNext}
			onAddAllToPlaylist={navidromeCallbacks.onAddAllToPlaylist}
		>
			{#snippet icon()}
				<NavidromeIcon class="h-5 w-5" />
			{/snippet}
		</AlbumSourceBar>
	{/if}
{/if}

{#if plexEnabled}
	{#if loadingPlex}
		<div class="skeleton h-14 w-full rounded-2xl"></div>
	{:else if plexMatch?.found}
		<AlbumSourceBar
			sourceLabel="Plex"
			sourceColor="rgb(var(--brand-plex))"
			trackCount={plexMatch.tracks.length}
			totalTracks={tracksInfo.tracks.length}
			onPlayAll={plexCallbacks.onPlayAll}
			onShuffle={plexCallbacks.onShuffle}
			onAddAllToQueue={plexCallbacks.onAddAllToQueue}
			onPlayAllNext={plexCallbacks.onPlayAllNext}
			onAddAllToPlaylist={plexCallbacks.onAddAllToPlaylist}
		>
			{#snippet icon()}
				<PlexIcon class="h-5 w-5" />
			{/snippet}
		</AlbumSourceBar>
	{/if}
{/if}
