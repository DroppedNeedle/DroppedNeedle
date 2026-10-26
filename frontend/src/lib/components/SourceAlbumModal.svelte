<script lang="ts">
	import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
	import { remoteApi } from '$lib/queries/remotes/remoteApi';
	import { fetchLocalAlbumMatch } from '$lib/queries/albumMatch';
	import { Shuffle, Play, ListPlus, ListStart, ListMusic, Info, Download } from 'lucide-svelte';
	import { goto } from '$app/navigation';
	import { LocalDownloadsApi } from '$lib/queries/local/LocalDownloadsApi';
	import { gatewayStreamUrl } from '$lib/player/playbackGateway';
	import { downloadAlbumArchive, downloadTrackFile } from '$lib/utils/downloadActions';
	import { playerStore } from '$lib/stores/player.svelte';
	import { launchLocalPlayback } from '$lib/player/launchLocalPlayback';
	import { launchRemotePlayback } from '$lib/player/launchRemotePlayback';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import AddToPlaylistModal from '$lib/components/AddToPlaylistModal.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import MetadataPanel from '$lib/components/MetadataPanel.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import {
		buildQueueItem,
		buildQueueItemsFromLocal,
		buildRemoteQueueItems,
		compareDiscTrack,
		normalizeDiscNumber,
		recordingFor,
		REMOTE_FORMAT,
		remoteStreamKey,
		type TrackMeta,
		type TrackSourceData
	} from '$lib/player/queueHelpers';
	import type { QueueItem } from '$lib/player/types';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { withBasePath } from '$lib/utils/basePath';
	import { api } from '$lib/api/client';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import { Radio } from 'lucide-svelte';
	import type { LocalTrackInfo, LocalAlbumSummary } from '$lib/types';
	import type { RemoteAlbum, RemoteSource, RemoteTrack } from '$lib/queries/remotes/types';

	interface Props {
		open: boolean;
		sourceType: RemoteSource | 'local';
		album: RemoteAlbum | LocalAlbumSummary | null;
		onclose: () => void;
	}

	let { open = $bindable(), sourceType, album, onclose }: Props = $props();

	let remoteAlbum = $derived(album && 'source' in album ? album : null);
	let localAlbum = $derived(album && !('source' in album) ? album : null);
	let remoteTracks = $state<RemoteTrack[]>([]);
	let localTracks = $state<LocalTrackInfo[]>([]);
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

	let albumName = $derived(remoteAlbum?.title ?? localAlbum?.name ?? '');
	let artistName = $derived(album?.artist_name ?? '');
	let year = $derived(album?.year);
	let albumId = $derived(remoteAlbum?.id ?? localAlbum?.musicbrainz_id ?? '');
	let mbid = $derived(
		remoteAlbum ? (remoteAlbum.release_group_mbid ?? null) : (localAlbum?.musicbrainz_id ?? null)
	);
	let artistMbid = $derived(
		remoteAlbum ? (remoteAlbum.artist_mbid ?? null) : (localAlbum?.artist_mbid ?? null)
	);
	let albumCoverUrl = $derived(remoteAlbum?.image_url ?? localAlbum?.cover_url ?? '');
	let coverUrl = $derived(getCoverUrl(albumCoverUrl || null, mbid ?? albumId));
	let canNavigate = $derived(!!mbid || !!albumName);
	let albumHref = $derived(
		mbid
			? withBasePath(`/album/${mbid}`)
			: albumName
				? withBasePath(`/search?q=${encodeURIComponent(albumName)}`)
				: undefined
	);
	let canNavigateArtist = $derived(!!artistMbid);
	let trackCount = $derived(remoteAlbum ? remoteTracks.length : localTracks.length);
	// Album-level viewer permission gates both the bulk and per-track Download
	// items (mirrors the album-page menus); fail-open on a missing bit.
	let localDownloadAllowed = $derived(localAlbum?.download_allowed !== false);

	$effect(() => {
		if (open && album) {
			fetchTracks();
		}
		if (!open) {
			remoteTracks = [];
			localTracks = [];
			trackError = '';
		}
	});

	async function fetchTracks(): Promise<void> {
		const id = ++fetchId;
		loadingTracks = true;
		trackError = '';
		try {
			if (remoteAlbum) {
				const page = await remoteApi.albumTracks(remoteAlbum.source, remoteAlbum.id);
				if (id !== fetchId) return;
				remoteTracks = page.items;
			} else if (localAlbum) {
				const match = await fetchLocalAlbumMatch(localAlbum.musicbrainz_id);
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
		if (!albumHref) return;
		const target = albumHref;
		handleClose();
		goto(target);
	}

	function goToArtist(): void {
		if (!canNavigateArtist || !artistMbid) return;
		const target = artistMbid;
		handleClose();
		goto(withBasePath(`/artist/${target}`));
	}

	async function launchInstantMix() {
		const seed = remoteAlbum;
		if (seed?.source !== 'jellyfin') return;
		mixLoading = true;
		try {
			const mix = await api.v3.GET(REMOTE_ENDPOINTS.mix('jellyfin', seed.id, { limit: 50 }));
			if (mix.items.length > 0) {
				const items: QueueItem[] = mix.items.map((t) => ({
					trackSourceId: t.id,
					trackName: t.title,
					artistName: t.artist_name,
					trackNumber: t.track_number ?? 0,
					discNumber: normalizeDiscNumber(t.disc_number),
					albumId: t.album_id || '',
					albumName: t.album_name,
					coverUrl: t.image_url ?? null,
					sourceType: 'jellyfin' as const,
					streamUrl: gatewayStreamUrl('jellyfin', t.id),
					format: REMOTE_FORMAT,
					recordingIds: recordingFor('jellyfin', t.recording_mbid)
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
		const target = remoteAlbum;
		if (target?.source !== 'navidrome') return;
		infoLoading = true;
		infoOpen = true;
		try {
			const info = await api.v3.GET(REMOTE_ENDPOINTS.infoAlbum('navidrome', target.id));
			infoNotes = info.biography;
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

		if (remoteAlbum && remoteTracks.length > 0) {
			launchRemotePlayback([...remoteTracks].sort(compareDiscTrack), 0, shuffle, meta);
		} else if (localAlbum && localTracks.length > 0) {
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

		if (remoteAlbum) {
			const track = remoteTracks[index];
			if (track && remoteStreamKey(track) === null) return;
			const sorted = [...remoteTracks].sort(compareDiscTrack);
			const sortedIdx = track ? sorted.indexOf(track) : -1;
			launchRemotePlayback(sorted, Math.max(sortedIdx, 0), false, meta);
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
			coverUrl: albumCoverUrl || null,
			artistId: artistMbid ?? undefined
		};
	}

	function remoteTrackSlot(
		track: RemoteTrack
	): Pick<TrackSourceData, 'jellyfinTrack' | 'navidromeTrack' | 'plexTrack'> {
		if (track.source === 'jellyfin') return { jellyfinTrack: track };
		if (track.source === 'navidrome') return { navidromeTrack: track };
		return { plexTrack: track };
	}

	function buildTrackQueueItem(index: number): QueueItem | null {
		const meta = getTrackMeta();
		if (remoteAlbum) {
			const track = remoteTracks[index];
			if (!track) return null;
			const sourceData: TrackSourceData = {
				trackPosition: track.track_number ?? 0,
				discNumber: normalizeDiscNumber(track.disc_number),
				trackTitle: track.title,
				trackLength: track.duration_secs ?? 0,
				...remoteTrackSlot(track)
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
		if (remoteAlbum) {
			return buildRemoteQueueItems([...remoteTracks].sort(compareDiscTrack), meta);
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
		if (localAlbum && localAlbum.download_allowed !== false) {
			const albumMbid = localAlbum.musicbrainz_id;
			items.push({
				label: 'Download Album',
				icon: Download,
				onclick: () => void downloadAlbumArchive(LocalDownloadsApi.albumByMbid(albumMbid))
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
		if (localAlbum && localDownloadAllowed) {
			const track = localTracks[index];
			if (track) {
				items.push({
					label: 'Download',
					icon: Download,
					onclick: () =>
						void downloadTrackFile(LocalDownloadsApi.track(track.track_file_id), track.title)
				});
			}
		}
		return items;
	}

	function getTrackName(index: number): string {
		if (remoteAlbum) return remoteTracks[index]?.title ?? '';
		return localTracks[index]?.title ?? '';
	}

	function getTrackNumber(index: number): number {
		if (remoteAlbum) return remoteTracks[index]?.track_number ?? 0;
		return localTracks[index]?.track_number ?? 0;
	}

	function getTrackDiscNumber(index: number): number {
		if (remoteAlbum) return normalizeDiscNumber(remoteTracks[index]?.disc_number);
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

{#if album}
	<Modal bind:open title={albumName} titleHref={albumHref} size="xl" closeButton {onclose}>
		<div class="flex gap-4 sm:gap-5">
			<div class="shrink-0">
				{#if canNavigate}
					<button
						type="button"
						onclick={goToAlbum}
						class="block cursor-pointer overflow-hidden rounded-lg shadow-lg transition-shadow hover:shadow-xl focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
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
					<div class="overflow-hidden rounded-lg shadow-lg">
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

			<div class="flex min-w-0 flex-1 flex-col justify-center">
				{#if canNavigateArtist}
					<button
						type="button"
						onclick={goToArtist}
						class="cursor-pointer truncate text-left text-base opacity-70 transition-colors hover:text-accent"
					>
						{artistName}
					</button>
				{:else}
					<p class="truncate text-base opacity-70">{artistName}</p>
				{/if}
				<div class="mt-2 flex flex-wrap items-center gap-2">
					{#if year}
						<span class="badge badge-sm badge-ghost">{year}</span>
					{/if}
					{#if sourceType === 'jellyfin'}
						<span class="badge badge-sm badge-info">Jellyfin</span>
					{:else if sourceType === 'navidrome'}
						<span class="badge badge-sm badge-primary">Navidrome</span>
					{:else if sourceType === 'plex'}
						<span class="badge badge-sm badge-warning">Plex</span>
					{:else if localAlbum}
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
		</div>

		{#if trackCount > 0}
			<div class="mt-4 flex flex-wrap items-center gap-2">
				<Button variant="accent" size="sm" onclick={() => playAll(false)}>
					<Play class="size-4 fill-current" aria-hidden="true" />
					Play All
				</Button>
				<Button variant="ghost" size="sm" icon={Shuffle} onclick={() => playAll(true)}
					>Shuffle</Button
				>
				{#if sourceType === 'jellyfin'}
					<Button
						variant="outline"
						size="sm"
						icon={Radio}
						loading={mixLoading}
						onclick={launchInstantMix}>Instant Mix</Button
					>
				{/if}
				{#if sourceType === 'navidrome'}
					<Button
						variant="ghost"
						size="sm"
						icon={Info}
						disabled={infoLoading}
						onclick={openAlbumInfo}>Info</Button
					>
				{/if}
				<ContextMenu items={getBulkMenuItems()} position="end" size="sm" />
			</div>
		{/if}

		<div class="divider my-2"></div>

		<div class="max-h-112 overflow-y-auto">
			{#if loadingTracks}
				<div class="flex justify-center py-6">
					<span class="loading loading-spinner loading-md"></span>
				</div>
			{:else if trackError}
				<div role="alert" class="alert alert-error alert-soft">
					<span>{trackError}</span>
					<Button variant="ghost" size="sm" onclick={fetchTracks}>Try again</Button>
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
								type="button"
								class="flex items-center gap-3 flex-1 min-w-0 py-1.5 px-1 rounded-lg text-left group/track"
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
								{#if remoteAlbum}
									{@const dur = remoteTracks[i]?.duration_secs}
									{#if dur}
										<span class="text-xs opacity-40 shrink-0">{formatDuration(dur)}</span>
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
	</Modal>
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
