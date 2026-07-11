<script lang="ts">
	import { resolve } from '$app/paths';
	import { Shuffle, Play, X, ListPlus, ListStart, ListMusic, Info, Download } from 'lucide-svelte';
	import { goto } from '$app/navigation';
	import { API } from '$lib/constants';
	import { downloadFile } from '$lib/utils/downloadHelper';
	import { playerStore } from '$lib/stores/player.svelte';
	import { launchJellyfinPlayback } from '$lib/player/launchJellyfinPlayback';
	import { launchLocalPlayback } from '$lib/player/launchLocalPlayback';
	import { launchNavidromePlayback } from '$lib/player/launchNavidromePlayback';
	import { launchPlexPlayback } from '$lib/player/launchPlexPlayback';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import AddToPlaylistModal from '$lib/components/AddToPlaylistModal.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import MetadataPanel from '$lib/components/MetadataPanel.svelte';
	import {
		buildQueueItem,
		buildQueueItemsFromJellyfin,
		buildQueueItemsFromLocal,
		buildQueueItemsFromNavidrome,
		buildQueueItemsFromPlex,
		compareDiscTrack,
		normalizeCodec,
		normalizeDiscNumber,
		type TrackMeta,
		type TrackSourceData
	} from '$lib/player/queueHelpers';
	import type { QueueItem } from '$lib/player/types';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { api } from '$lib/api/client';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import AudioQualityBadge from '$lib/components/AudioQualityBadge.svelte';
	import { Radio } from 'lucide-svelte';
	import type {
		JellyfinTrackInfo,
		LocalAlbumMatch,
		LocalTrackInfo,
		NavidromeTrackInfo,
		PlexTrackInfo,
		NavidromeAlbumDetail,
		PlexAlbumDetail,
		JellyfinAlbumSummary,
		LocalAlbumSummary,
		NavidromeAlbumSummary,
		PlexAlbumSummary
	} from '$lib/types';

	type SourceType = 'jellyfin' | 'local' | 'navidrome' | 'plex';

	interface Props {
		open: boolean;
		sourceType: SourceType;
		album:
			| JellyfinAlbumSummary
			| LocalAlbumSummary
			| NavidromeAlbumSummary
			| PlexAlbumSummary
			| null;
		onclose: () => void;
	}

	let { open = $bindable(), sourceType, album, onclose }: Props = $props();

	let jellyfinTracks = $state<JellyfinTrackInfo[]>([]);
	let localTracks = $state<LocalTrackInfo[]>([]);
	let navidromeTracks = $state<NavidromeTrackInfo[]>([]);
	let plexTracks = $state<PlexTrackInfo[]>([]);
	let loadingTracks = $state(false);
	let trackError = $state('');
	let fetchId = 0;
	let playlistModalRef = $state<{ open: (tracks: QueueItem[]) => void } | null>(null);
	let mixLoading = $state(false);
	let infoOpen = $state(false);
	let infoNotes = $state('');
	let infoLastfmUrl = $state('');
	let infoMbid = $state('');
	let infoImageUrl = $state('');
	let infoLoading = $state(false);

	let albumName = $derived(album?.name ?? '');
	let artistName = $derived(album?.artist_name ?? '');
	let year = $derived(album?.year);
	let albumId = $derived(getAlbumId());
	let mbid = $derived(getMbid());
	let artistMbid = $derived(getArtistMbid());
	let coverUrl = $derived(getCoverUrl(getAlbumCoverUrl() || null, mbid ?? albumId));
	let canNavigate = $derived(!!mbid || !!albumName);
	let canNavigateArtist = $derived(!!artistMbid);
	let trackCount = $derived(
		sourceType === 'jellyfin'
			? jellyfinTracks.length
			: sourceType === 'navidrome'
				? navidromeTracks.length
				: sourceType === 'plex'
					? plexTracks.length
					: localTracks.length
	);

	function getAlbumCoverUrl(): string {
		if (!album) return '';
		if (sourceType === 'jellyfin') {
			return (album as JellyfinAlbumSummary).image_url ?? '';
		}
		if (sourceType === 'navidrome') {
			return (album as NavidromeAlbumSummary).image_url ?? '';
		}
		if (sourceType === 'plex') {
			return (album as PlexAlbumSummary).image_url ?? '';
		}
		return (album as LocalAlbumSummary).cover_url ?? '';
	}

	function getAlbumId(): string {
		if (!album) return '';
		if (sourceType === 'jellyfin') return (album as JellyfinAlbumSummary).jellyfin_id;
		if (sourceType === 'navidrome') return (album as NavidromeAlbumSummary).navidrome_id;
		if (sourceType === 'plex') return (album as PlexAlbumSummary).plex_id;
		return (album as LocalAlbumSummary).musicbrainz_id;
	}

	function getMbid(): string | null {
		if (!album) return null;
		if (sourceType === 'jellyfin') return (album as JellyfinAlbumSummary).musicbrainz_id ?? null;
		if (sourceType === 'navidrome') return (album as NavidromeAlbumSummary).musicbrainz_id ?? null;
		if (sourceType === 'plex') return (album as PlexAlbumSummary).musicbrainz_id ?? null;
		return (album as LocalAlbumSummary).musicbrainz_id ?? null;
	}

	function getArtistMbid(): string | null {
		if (!album) return null;
		if (sourceType === 'jellyfin')
			return (album as JellyfinAlbumSummary).artist_musicbrainz_id ?? null;
		if (sourceType === 'navidrome')
			return (album as NavidromeAlbumSummary).artist_musicbrainz_id ?? null;
		if (sourceType === 'plex') return (album as PlexAlbumSummary).artist_musicbrainz_id ?? null;
		return (album as LocalAlbumSummary).artist_mbid ?? null;
	}

	$effect(() => {
		if (open && album) {
			fetchTracks();
		}
		if (!open) {
			jellyfinTracks = [];
			localTracks = [];
			navidromeTracks = [];
			plexTracks = [];
			trackError = '';
		}
	});

	async function fetchTracks(): Promise<void> {
		const id = ++fetchId;
		loadingTracks = true;
		trackError = '';
		try {
			if (sourceType === 'jellyfin') {
				const jfAlbum = album as JellyfinAlbumSummary;
				const data = await api.global.get<JellyfinTrackInfo[]>(
					API.jellyfinLibrary.albumTracks(jfAlbum.jellyfin_id)
				);
				if (id !== fetchId) return;
				jellyfinTracks = data;
			} else if (sourceType === 'navidrome') {
				const ndAlbum = album as NavidromeAlbumSummary;
				const detail = await api.global.get<NavidromeAlbumDetail>(
					API.navidromeLibrary.albumDetail(ndAlbum.navidrome_id)
				);
				if (id !== fetchId) return;
				navidromeTracks = detail.tracks ?? [];
			} else if (sourceType === 'plex') {
				const plexAlbum = album as PlexAlbumSummary;
				const detail = await api.global.get<PlexAlbumDetail>(
					API.plexLibrary.albumDetail(plexAlbum.plex_id)
				);
				if (id !== fetchId) return;
				plexTracks = detail.tracks ?? [];
			} else {
				const localAlbum = album as LocalAlbumSummary;
				const match = await api.global.get<LocalAlbumMatch>(
					API.local.albumMatch(localAlbum.musicbrainz_id)
				);
				if (id !== fetchId) return;
				localTracks = match.tracks;
			}
		} catch (e) {
			if (id === fetchId)
				trackError = e instanceof Error ? e.message : "Couldn't load the track list.";
		} finally {
			if (id === fetchId) loadingTracks = false;
		}
	}

	function handleClose(): void {
		open = false;
		onclose();
	}

	function goToAlbum(): void {
		if (!canNavigate) return;
		if (mbid) {
			const target = mbid;
			handleClose();
			goto(resolve(`/album/${target}`));
		} else if (albumName) {
			handleClose();
			goto(resolve(`/search?q=${encodeURIComponent(albumName)}`));
		}
	}

	function goToArtist(): void {
		if (!canNavigateArtist || !artistMbid) return;
		const target = artistMbid;
		handleClose();
		goto(resolve(`/artist/${target}`));
	}

	async function launchInstantMix() {
		if (sourceType !== 'jellyfin' || !album || !('jellyfin_id' in album)) return;
		mixLoading = true;
		try {
			const tracks = await api.get<JellyfinTrackInfo[]>(
				API.jellyfinLibrary.instantMix(album.jellyfin_id)
			);
			if (tracks.length > 0) {
				const items: QueueItem[] = tracks.map((t) => ({
					trackSourceId: t.jellyfin_id,
					trackName: t.title,
					artistName: t.artist_name,
					trackNumber: t.track_number,
					discNumber: normalizeDiscNumber(t.disc_number),
					albumId: t.album_id || '',
					albumName: t.album_name,
					coverUrl: t.album_id ? `/api/v1/jellyfin/image/${t.album_id}` : null,
					sourceType: 'jellyfin' as const,
					streamUrl: API.stream.jellyfin(t.jellyfin_id),
					format: normalizeCodec(t.codec)
				}));
				playerStore.playQueue(items, 0, false);
			}
		} catch {
			return;
		} finally {
			mixLoading = false;
		}
	}

	async function openAlbumInfo() {
		if (sourceType !== 'navidrome' || !album || !('navidrome_id' in album)) return;
		infoLoading = true;
		infoOpen = true;
		try {
			const info = await api.get<{
				notes: string;
				musicbrainz_id: string;
				lastfm_url: string;
				image_url: string;
			}>(API.navidromeLibrary.albumInfo(album.navidrome_id));
			infoNotes = info.notes;
			infoLastfmUrl = info.lastfm_url;
			infoMbid = info.musicbrainz_id;
			infoImageUrl = info.image_url;
		} catch {
			infoNotes = '';
		} finally {
			infoLoading = false;
		}
	}

	function playAll(shuffle: boolean = false): void {
		if (!album) return;
		const meta = {
			albumId: mbid ?? albumId,
			albumName,
			artistName,
			coverUrl
		};

		if (sourceType === 'jellyfin' && jellyfinTracks.length > 0) {
			launchJellyfinPlayback([...jellyfinTracks].sort(compareDiscTrack), 0, shuffle, meta);
		} else if (sourceType === 'navidrome' && navidromeTracks.length > 0) {
			launchNavidromePlayback([...navidromeTracks].sort(compareDiscTrack), 0, shuffle, meta);
		} else if (sourceType === 'plex' && plexTracks.length > 0) {
			launchPlexPlayback([...plexTracks].sort(compareDiscTrack), 0, shuffle, meta);
		} else if (sourceType === 'local' && localTracks.length > 0) {
			launchLocalPlayback([...localTracks].sort(compareDiscTrack), 0, shuffle, meta);
		}
	}

	function playTrack(index: number): void {
		if (!album) return;
		const meta = {
			albumId: mbid ?? albumId,
			albumName,
			artistName,
			coverUrl
		};

		if (sourceType === 'jellyfin') {
			const sorted = [...jellyfinTracks].sort(compareDiscTrack);
			const track = jellyfinTracks[index];
			const sortedIdx = track ? sorted.indexOf(track) : index;
			launchJellyfinPlayback(sorted, sortedIdx >= 0 ? sortedIdx : index, false, meta);
		} else if (sourceType === 'navidrome') {
			const sorted = [...navidromeTracks].sort(compareDiscTrack);
			const track = navidromeTracks[index];
			const sortedIdx = track ? sorted.indexOf(track) : index;
			launchNavidromePlayback(sorted, sortedIdx >= 0 ? sortedIdx : index, false, meta);
		} else if (sourceType === 'plex') {
			const streamable = plexTracks.filter((t) => t.part_key);
			const sorted = [...streamable].sort(compareDiscTrack);
			const track = plexTracks[index];
			if (track && !track.part_key) return;
			const sortedIdx = track ? sorted.indexOf(track) : -1;
			if (sortedIdx === -1 && sorted.length > 0) {
				launchPlexPlayback(sorted, 0, false, meta);
			} else if (sortedIdx >= 0) {
				launchPlexPlayback(sorted, sortedIdx, false, meta);
			}
		} else {
			const sorted = [...localTracks].sort(compareDiscTrack);
			const track = localTracks[index];
			const sortedIdx = track ? sorted.indexOf(track) : index;
			launchLocalPlayback(sorted, sortedIdx >= 0 ? sortedIdx : index, false, meta);
		}
	}

	function getTrackMeta(): TrackMeta {
		return {
			albumId: mbid ?? albumId,
			albumName,
			artistName,
			coverUrl: getAlbumCoverUrl() || null,
			artistId: artistMbid ?? undefined
		};
	}

	function buildTrackQueueItem(index: number): QueueItem | null {
		const meta = getTrackMeta();
		if (sourceType === 'jellyfin') {
			const track = jellyfinTracks[index];
			if (!track) return null;
			const sourceData: TrackSourceData = {
				trackPosition: track.track_number,
				discNumber: normalizeDiscNumber(track.disc_number),
				trackTitle: track.title,
				trackLength: track.duration_seconds,
				jellyfinTrack: track
			};
			return buildQueueItem(meta, sourceData);
		}

		if (sourceType === 'navidrome') {
			const track = navidromeTracks[index];
			if (!track) return null;
			const sourceData: TrackSourceData = {
				trackPosition: track.track_number,
				discNumber: normalizeDiscNumber(track.disc_number),
				trackTitle: track.title,
				trackLength: track.duration_seconds,
				navidromeTrack: track
			};
			return buildQueueItem(meta, sourceData);
		}

		if (sourceType === 'plex') {
			const track = plexTracks[index];
			if (!track) return null;
			const sourceData: TrackSourceData = {
				trackPosition: track.track_number,
				discNumber: normalizeDiscNumber(track.disc_number),
				trackTitle: track.title,
				trackLength: track.duration_seconds,
				plexTrack: track
			};
			return buildQueueItem(meta, sourceData);
		}

		const track = localTracks[index];
		if (!track) return null;
		const sourceData: TrackSourceData = {
			trackPosition: track.track_number,
			discNumber: normalizeDiscNumber(track.disc_number),
			trackTitle: track.title,
			trackLength: track.duration_seconds ?? undefined,
			localTrack: track
		};
		return buildQueueItem(meta, sourceData);
	}

	function buildAlbumQueueItems(): QueueItem[] {
		const meta = getTrackMeta();
		if (sourceType === 'jellyfin') {
			return buildQueueItemsFromJellyfin([...jellyfinTracks].sort(compareDiscTrack), meta);
		}
		if (sourceType === 'navidrome') {
			return buildQueueItemsFromNavidrome([...navidromeTracks].sort(compareDiscTrack), meta);
		}
		if (sourceType === 'plex') {
			return buildQueueItemsFromPlex([...plexTracks].sort(compareDiscTrack), meta);
		}
		return buildQueueItemsFromLocal([...localTracks].sort(compareDiscTrack), meta);
	}

	function addAllToQueue(): void {
		const queueItems = buildAlbumQueueItems();
		if (queueItems.length === 0) return;
		playerStore.addMultipleToQueue(queueItems);
	}

	function playAllNext(): void {
		const queueItems = buildAlbumQueueItems();
		if (queueItems.length === 0) return;
		playerStore.playMultipleNext(queueItems);
	}

	function addAllToPlaylist(): void {
		const queueItems = buildAlbumQueueItems();
		if (queueItems.length === 0) return;
		playlistModalRef?.open(queueItems);
	}

	function addTrackToPlaylist(index: number): void {
		const queueItem = buildTrackQueueItem(index);
		if (!queueItem) return;
		playlistModalRef?.open([queueItem]);
	}

	function getBulkMenuItems(): MenuItem[] {
		const items: MenuItem[] = [
			{ label: 'Add All to Queue', icon: ListPlus, onclick: addAllToQueue },
			{ label: 'Play All Next', icon: ListStart, onclick: playAllNext },
			{ label: 'Add All to Playlist', icon: ListMusic, onclick: addAllToPlaylist }
		];
		if (sourceType === 'local' && album) {
			const localAlbum = album as import('$lib/types').LocalAlbumSummary;
			items.push({
				label: 'Download Album',
				icon: Download,
				onclick: () => downloadFile(API.download.localAlbumByMbid(localAlbum.musicbrainz_id))
			});
		}
		return items;
	}

	function getTrackContextMenuItems(index: number): MenuItem[] {
		const queueItem = buildTrackQueueItem(index);
		const hasQueueItem = queueItem !== null;
		const items: MenuItem[] = [
			{
				label: 'Add to Queue',
				icon: ListPlus,
				onclick: () => {
					if (queueItem) playerStore.addToQueue(queueItem);
				},
				disabled: !hasQueueItem
			},
			{
				label: 'Play Next',
				icon: ListStart,
				onclick: () => {
					if (queueItem) playerStore.playNext(queueItem);
				},
				disabled: !hasQueueItem
			},
			{
				label: 'Add to Playlist',
				icon: ListMusic,
				onclick: () => addTrackToPlaylist(index),
				disabled: !hasQueueItem
			}
		];
		if (sourceType === 'local') {
			const track = localTracks[index];
			if (track) {
				items.push({
					label: 'Download',
					icon: Download,
					onclick: () => downloadFile(API.download.localTrack(track.track_file_id))
				});
			}
		}
		return items;
	}

	function getTrackName(index: number): string {
		if (sourceType === 'jellyfin') return jellyfinTracks[index]?.title ?? '';
		if (sourceType === 'navidrome') return navidromeTracks[index]?.title ?? '';
		if (sourceType === 'plex') return plexTracks[index]?.title ?? '';
		return localTracks[index]?.title ?? '';
	}

	function getTrackNumber(index: number): number {
		if (sourceType === 'jellyfin') return jellyfinTracks[index]?.track_number ?? 0;
		if (sourceType === 'navidrome') return navidromeTracks[index]?.track_number ?? 0;
		if (sourceType === 'plex') return plexTracks[index]?.track_number ?? 0;
		return localTracks[index]?.track_number ?? 0;
	}

	function getTrackDiscNumber(index: number): number {
		if (sourceType === 'jellyfin') return normalizeDiscNumber(jellyfinTracks[index]?.disc_number);
		if (sourceType === 'navidrome') return normalizeDiscNumber(navidromeTracks[index]?.disc_number);
		if (sourceType === 'plex') return normalizeDiscNumber(plexTracks[index]?.disc_number);
		return normalizeDiscNumber(localTracks[index]?.disc_number);
	}

	function formatDuration(seconds?: number | null): string {
		if (seconds == null) return '';
		const m = Math.floor(seconds / 60);
		const s = Math.floor(seconds % 60);
		return `${m}:${s.toString().padStart(2, '0')}`;
	}

	function formatSize(bytes: number): string {
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
		return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	}

	function isTrackPlaying(trackNum: number, discNumber: number = 1): boolean {
		return (
			playerStore.nowPlaying?.albumId === (mbid ?? albumId) &&
			(playerStore.currentQueueItem?.discNumber ?? 1) === discNumber &&
			playerStore.currentQueueItem?.trackNumber === trackNum &&
			playerStore.isPlaying
		);
	}
</script>

{#if open && album}
	<dialog class="modal modal-open">
		<div class="modal-box max-w-4xl p-0 overflow-hidden">
			<div class="flex gap-5 p-6 pb-4">
				<div class="shrink-0">
					{#if canNavigate}
						<button
							onclick={goToAlbum}
							class="block rounded-lg overflow-hidden shadow-lg hover:shadow-xl transition-shadow cursor-pointer"
							aria-label="Go to album"
						>
							<AlbumImage
								mbid={mbid ?? albumId}
								customUrl={coverUrl || null}
								alt={albumName}
								size="xl"
								requestSize={250}
								rounded="none"
								className="w-36 h-36"
							/>
						</button>
					{:else}
						<div class="rounded-lg overflow-hidden shadow-lg">
							<AlbumImage
								mbid={albumId}
								customUrl={coverUrl || null}
								alt={albumName}
								size="xl"
								requestSize={250}
								rounded="none"
								className="w-36 h-36"
							/>
						</div>
					{/if}
				</div>

				<div class="flex flex-col justify-center min-w-0 flex-1">
					{#if canNavigate}
						<button
							onclick={goToAlbum}
							class="text-xl font-bold truncate text-left hover:text-accent transition-colors cursor-pointer"
						>
							{albumName}
						</button>
					{:else}
						<h3 class="text-xl font-bold truncate">{albumName}</h3>
					{/if}
					{#if canNavigateArtist}
						<button
							onclick={goToArtist}
							class="text-base opacity-70 truncate text-left hover:text-accent transition-colors cursor-pointer"
						>
							{artistName}
						</button>
					{:else}
						<p class="text-base opacity-70 truncate">{artistName}</p>
					{/if}
					<div class="flex items-center gap-2 mt-2 flex-wrap">
						{#if year}
							<span class="badge badge-sm badge-ghost">{year}</span>
						{/if}
						{#if sourceType === 'jellyfin'}
							<span class="badge badge-sm badge-info">Jellyfin</span>
						{:else if sourceType === 'navidrome'}
							<span class="badge badge-sm badge-primary">Navidrome</span>
						{:else if sourceType === 'plex'}
							<span class="badge badge-sm badge-warning">Plex</span>
						{:else}
							{@const localAlbum = album as LocalAlbumSummary}
							<span class="badge badge-sm badge-accent">Local</span>
							{#if localAlbum.primary_format}
								<span class="badge badge-sm badge-ghost">{localAlbum.primary_format}</span>
							{/if}
							{#if localAlbum.total_size_bytes > 0}
								<span class="badge badge-sm badge-ghost"
									>{formatSize(localAlbum.total_size_bytes)}</span
								>
							{/if}
						{/if}
						{#if !mbid && albumName}
							<span class="badge badge-sm badge-info badge-outline">Search result</span>
						{/if}
					</div>
				</div>

				<button class="btn btn-sm btn-circle btn-ghost self-start -mr-2 -mt-2" onclick={handleClose}
					><X class="h-4 w-4" /></button
				>
			</div>

			<div class="flex items-center gap-2 px-6 pb-4 flex-wrap">
				{#if trackCount > 0}
					<button class="btn btn-sm btn-accent gap-1" onclick={() => playAll(false)}>
						<Play class="h-4 w-4 fill-current" />
						Play All
					</button>
					<button class="btn btn-sm btn-ghost gap-1" onclick={() => playAll(true)}>
						<Shuffle class="h-4 w-4" />
						Shuffle
					</button>
					{#if sourceType === 'jellyfin'}
						<button
							class="btn btn-sm btn-outline gap-1"
							onclick={launchInstantMix}
							disabled={mixLoading}
						>
							<span class:animate-pulse={mixLoading}>
								<Radio class="h-4 w-4" />
							</span>
							Instant Mix
						</button>
					{/if}
					{#if sourceType === 'navidrome'}
						<button
							class="btn btn-sm btn-ghost gap-1"
							onclick={openAlbumInfo}
							disabled={infoLoading}
						>
							<Info class="h-4 w-4" />
							Info
						</button>
					{/if}
					<ContextMenu items={getBulkMenuItems()} position="end" size="sm" />
				{/if}
			</div>

			<div class="divider my-0 px-6"></div>

			<div class="px-6 pt-3 pb-5 max-h-112 overflow-y-auto">
				{#if loadingTracks}
					<div class="flex justify-center py-6">
						<span class="loading loading-spinner loading-md"></span>
					</div>
				{:else if trackError}
					<div role="alert" class="alert alert-error alert-soft">
						<span>{trackError}</span>
						<button class="btn btn-sm btn-ghost" onclick={fetchTracks}>Try again</button>
					</div>
				{:else if trackCount > 0}
					<div class="flex flex-col">
						{#each { length: trackCount } as _, i (`track-${i}`)}
							{@const trackNum = getTrackNumber(i)}
							{@const discNum = getTrackDiscNumber(i)}
							{@const playing = isTrackPlaying(trackNum, discNum)}
							<div
								class="group/row flex items-center gap-2 w-full py-1 px-1 rounded-lg transition-colors {playing
									? 'bg-accent/10'
									: 'hover:bg-base-200'}"
							>
								<button
									class="flex items-center gap-3 flex-1 py-1.5 px-1 rounded-lg text-left group/track"
									onclick={() => playTrack(i)}
								>
									<span
										class="font-mono w-6 text-right text-sm shrink-0 {playing
											? 'text-accent'
											: 'opacity-40'}"
									>
										{#if playing}
											<NowPlayingIndicator />
										{:else}
											{trackNum}
										{/if}
									</span>
									<span class="text-sm truncate flex-1 {playing ? 'text-accent' : ''}"
										>{getTrackName(i)}</span
									>
									{#if sourceType === 'jellyfin'}
										{@const dur = jellyfinTracks[i]?.duration_seconds}
										{#if dur}
											<span class="text-xs opacity-40 shrink-0">{formatDuration(dur)}</span>
										{/if}
									{:else if sourceType === 'navidrome'}
										{@const dur = navidromeTracks[i]?.duration_seconds}
										{#if dur}
											<span class="text-xs opacity-40 shrink-0">{formatDuration(dur)}</span>
										{/if}
									{:else if sourceType === 'plex'}
										{@const dur = plexTracks[i]?.duration_seconds}
										{@const pt = plexTracks[i]}
										{#if dur}
											<span class="text-xs opacity-40 shrink-0">{formatDuration(dur)}</span>
										{/if}
										{#if pt}
											<span class="hidden sm:inline-flex shrink-0">
												<AudioQualityBadge
													codec={pt.codec}
													bitrate={pt.bitrate}
													audioChannels={pt.audio_channels}
													container={pt.container}
													compact
												/>
											</span>
										{/if}
									{:else}
										{@const lt = localTracks[i]}
										{#if lt?.duration_seconds}
											<span class="text-xs opacity-40 shrink-0"
												>{formatDuration(lt.duration_seconds)}</span
											>
										{/if}
									{/if}
									<span class={playing ? 'text-accent' : ''}>
										<Play
											class="h-4 w-4 shrink-0 transition-opacity {playing
												? 'opacity-100'
												: 'text-accent opacity-0 group-hover/track:opacity-100'} fill-current"
										/>
									</span>
								</button>
								<div>
									<ContextMenu items={getTrackContextMenuItems(i)} position="end" size="xs" />
								</div>
							</div>
						{/each}
					</div>
				{:else}
					<p class="text-sm opacity-50 text-center py-6">No tracks found for this album.</p>
				{/if}
			</div>
		</div>

		<form method="dialog" class="modal-backdrop">
			<button onclick={handleClose}>Close</button>
		</form>
	</dialog>
{/if}

<AddToPlaylistModal bind:this={playlistModalRef} />

<MetadataPanel
	bind:open={infoOpen}
	title={albumName}
	notes={infoNotes}
	imageUrl={infoImageUrl}
	lastfmUrl={infoLastfmUrl}
	musicbrainzId={infoMbid}
	onclose={() => (infoOpen = false)}
/>
