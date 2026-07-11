<script lang="ts">
	import { resolve } from '$app/paths';
	import {
		Shuffle,
		Pencil,
		Trash2,
		Play,
		X,
		ExternalLink,
		ListPlus,
		ListStart,
		ListMusic,
		Search
	} from 'lucide-svelte';
	import { goto } from '$app/navigation';
	import { API, TOAST_DURATION } from '$lib/constants';
	import { colors } from '$lib/colors';
	import { toastStore } from '$lib/stores/toast';
	import { playerStore } from '$lib/stores/player.svelte';
	import { launchYouTubePlayback } from '$lib/player/launchYouTubePlayback';
	import { launchTrackPlayback } from '$lib/player/launchTrackPlayback';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { api } from '$lib/api/client';
	import {
		buildQueueItemFromYouTube,
		buildQueueItemsFromYouTube,
		compareDiscTrack,
		getDiscTrackKey,
		normalizeDiscNumber,
		type TrackMeta
	} from '$lib/player/queueHelpers';
	import { openGlobalPlaylistModal } from '$lib/stores/playlistModal.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import type { YouTubeLink, YouTubeTrackLink } from '$lib/types';

	interface Props {
		open: boolean;
		link: YouTubeLink | null;
		onclose: () => void;
		onedit: (link: YouTubeLink) => void;
		ondelete: (albumId: string) => void;
	}

	let { open = $bindable(), link, onclose, onedit, ondelete }: Props = $props();

	let tracks = $state<YouTubeTrackLink[]>([]);
	let loadingTracks = $state(false);
	let trackError = $state(false);
	let confirmingDelete = $state(false);
	let deleting = $state(false);
	let fetchId = 0;

	let canNavigate = $derived(!link?.is_manual && !!link?.album_id);
	let trackSections = $derived.by(() => getTrackSections());

	function getSortedTracks(): YouTubeTrackLink[] {
		return [...tracks].sort(compareDiscTrack);
	}

	function getTrackSections(): Array<{ discNumber: number; tracks: YouTubeTrackLink[] }> {
		// eslint-disable-next-line svelte/prefer-svelte-reactivity
		const sections = new Map<number, YouTubeTrackLink[]>();
		for (const track of getSortedTracks()) {
			const discNumber = normalizeDiscNumber(track.disc_number);
			const existing = sections.get(discNumber);
			if (existing) {
				existing.push(track);
			} else {
				sections.set(discNumber, [track]);
			}
		}
		return Array.from(sections.entries()).map(([discNumber, discTracks]) => ({
			discNumber,
			tracks: discTracks
		}));
	}

	function getTrackMeta(): TrackMeta | null {
		if (!link) return null;
		return {
			albumId: link.album_id,
			albumName: link.album_name,
			artistName: link.artist_name,
			coverUrl: getCoverUrl(link.cover_url, link.album_id)
		};
	}

	$effect(() => {
		if (open && link) {
			confirmingDelete = false;
			deleting = false;
			fetchTracks(link.album_id);
		}
		if (!open) {
			tracks = [];
		}
	});

	async function fetchTracks(albumId: string): Promise<void> {
		const id = ++fetchId;
		loadingTracks = true;
		trackError = false;
		try {
			const data = await api.global.get<YouTubeTrackLink[]>(API.youtube.trackLinks(albumId));
			if (id !== fetchId) return;
			tracks = data.sort(compareDiscTrack);
		} catch (_e) {
			if (id === fetchId) {
				trackError = true;
			}
		} finally {
			if (id === fetchId) loadingTracks = false;
		}
	}

	function handleClose(): void {
		open = false;
		onclose();
	}

	function goToAlbum(): void {
		if (!canNavigate || !link) return;
		const albumId = link.album_id;
		handleClose();
		goto(resolve(`/album/${albumId}`));
	}

	function searchArtist(): void {
		if (!link) return;
		handleClose();
		goto(resolve(`/search?q=${encodeURIComponent(link.artist_name)}`));
	}

	async function playFullAlbum(): Promise<void> {
		if (!link?.video_id) return;
		try {
			await launchYouTubePlayback(
				{
					albumId: link.album_id,
					albumName: link.album_name,
					artistName: link.artist_name,
					coverUrl: getCoverUrl(link.cover_url, link.album_id),
					videoId: link.video_id,
					embedUrl: link.embed_url ?? undefined
				},
				{
					onLoadError: () => {
						toastStore.show({
							message: "Couldn't load the video",
							type: 'error',
							duration: TOAST_DURATION
						});
					}
				}
			);
		} catch {
			toastStore.show({
				message: "Couldn't play the album video",
				type: 'error',
				duration: TOAST_DURATION
			});
		}
	}

	function playAllTracks(shuffle: boolean = false): void {
		if (!link || tracks.length === 0) return;
		launchTrackPlayback(getSortedTracks(), 0, shuffle, {
			albumId: link.album_id,
			albumName: link.album_name,
			artistName: link.artist_name,
			coverUrl: getCoverUrl(link.cover_url, link.album_id)
		});
	}

	function playTrack(track: YouTubeTrackLink): void {
		if (!link) return;
		const sortedTracks = getSortedTracks();
		const idx = sortedTracks.findIndex((item) => getDiscTrackKey(item) === getDiscTrackKey(track));
		if (idx === -1) return;
		launchTrackPlayback(sortedTracks, idx, false, {
			albumId: link.album_id,
			albumName: link.album_name,
			artistName: link.artist_name,
			coverUrl: getCoverUrl(link.cover_url, link.album_id)
		});
	}

	function addAllToQueue(): void {
		const meta = getTrackMeta();
		if (!meta || tracks.length === 0) return;
		const items = buildQueueItemsFromYouTube(getSortedTracks(), meta);
		playerStore.addMultipleToQueue(items);
	}

	function addAllToPlaylist(): void {
		const meta = getTrackMeta();
		if (!meta || tracks.length === 0) return;
		const items = buildQueueItemsFromYouTube(getSortedTracks(), meta);
		openGlobalPlaylistModal(items);
	}

	function getTrackMenuItems(track: YouTubeTrackLink): MenuItem[] {
		const meta = getTrackMeta();
		if (!meta) return [];
		return [
			{
				label: 'Play Next',
				icon: ListStart,
				onclick: () => {
					const item = buildQueueItemFromYouTube(track, meta);
					playerStore.playNext(item);
				}
			},
			{
				label: 'Add to Queue',
				icon: ListPlus,
				onclick: () => {
					const item = buildQueueItemFromYouTube(track, meta);
					playerStore.addToQueue(item);
				}
			},
			{
				label: 'Add to Playlist',
				icon: ListMusic,
				onclick: () => {
					const item = buildQueueItemFromYouTube(track, meta);
					openGlobalPlaylistModal([item]);
				}
			}
		];
	}

	function getBulkMenuItems(): MenuItem[] {
		return [
			{
				label: 'Play All Next',
				icon: ListStart,
				onclick: () => {
					const meta = getTrackMeta();
					if (!meta || tracks.length === 0) return;
					const items = buildQueueItemsFromYouTube(getSortedTracks(), meta);
					playerStore.playMultipleNext(items);
				},
				disabled: tracks.length === 0
			},
			{
				label: 'Add All to Queue',
				icon: ListPlus,
				onclick: addAllToQueue,
				disabled: tracks.length === 0
			},
			{
				label: 'Add All to Playlist',
				icon: ListMusic,
				onclick: addAllToPlaylist,
				disabled: tracks.length === 0
			}
		];
	}

	function handleEdit(): void {
		if (!link) return;
		onedit(link);
	}

	async function handleDelete(): Promise<void> {
		if (!link) return;
		if (!confirmingDelete) {
			confirmingDelete = true;
			return;
		}
		deleting = true;
		try {
			await api.global.delete(API.youtube.deleteLink(link.album_id));
			toastStore.show({ message: 'Link removed', type: 'success', duration: TOAST_DURATION });
			open = false;
			ondelete(link.album_id);
		} catch {
			toastStore.show({
				message: 'Failed to delete link',
				type: 'error',
				duration: TOAST_DURATION
			});
		} finally {
			deleting = false;
			confirmingDelete = false;
		}
	}

	function formatDate(iso: string): string {
		return new Date(iso).toLocaleDateString(undefined, {
			year: 'numeric',
			month: 'short',
			day: 'numeric'
		});
	}
</script>

{#if open && link}
	<dialog class="modal modal-open">
		<div class="modal-box max-w-5xl p-0 overflow-hidden">
			<div class="flex gap-6 p-6 pb-4">
				<div class="shrink-0">
					{#if canNavigate}
						<button
							onclick={goToAlbum}
							class="block rounded-xl overflow-hidden shadow-lg hover:shadow-xl transition-shadow cursor-pointer ring-1 ring-base-content/10"
							aria-label="Go to album"
						>
							<AlbumImage
								mbid={link.album_id}
								customUrl={link.cover_url}
								alt={link.album_name}
								size="xl"
								requestSize={250}
								rounded="none"
								className="w-52 h-52"
							/>
						</button>
					{:else}
						<div class="rounded-xl overflow-hidden shadow-lg ring-1 ring-base-content/10">
							<AlbumImage
								mbid={link.album_id}
								customUrl={link.cover_url}
								alt={link.album_name}
								size="xl"
								requestSize={250}
								rounded="none"
								className="w-52 h-52"
							/>
						</div>
					{/if}
				</div>

				<div class="flex flex-col justify-between min-w-0 flex-1 py-1">
					<div class="flex-1 min-w-0">
						{#if canNavigate}
							<button
								onclick={goToAlbum}
								class="text-2xl font-bold truncate text-left hover:text-accent transition-colors cursor-pointer block max-w-full"
							>
								{link.album_name}
							</button>
						{:else}
							<h3 class="text-2xl font-bold truncate">{link.album_name}</h3>
						{/if}
						<button
							onclick={searchArtist}
							class="text-base opacity-70 truncate hover:text-accent transition-colors cursor-pointer flex items-center gap-1.5 mt-0.5 group/artist"
						>
							{link.artist_name}
							<Search class="h-3 w-3 opacity-0 group-hover/artist:opacity-100 transition-opacity" />
						</button>
						<div class="flex items-center gap-3 mt-3 flex-wrap">
							<div class="flex items-center gap-1.5">
								<YouTubeIcon class="h-4 w-4 text-red-500" />
								<span class="text-xs opacity-50">{formatDate(link.created_at)}</span>
							</div>
							{#if link.is_manual}
								<span class="badge badge-xs badge-ghost">Manual</span>
							{/if}
							{#if tracks.length > 0}
								<span class="badge badge-sm badge-accent">{tracks.length} tracks</span>
							{/if}
						</div>
					</div>

					<div class="flex items-center gap-2 mt-4 flex-wrap">
						{#if tracks.length > 0}
							<button class="btn btn-sm btn-accent gap-1.5" onclick={() => playAllTracks(false)}>
								<Play class="h-4 w-4 fill-current" />
								Play
							</button>
							<button class="btn btn-sm btn-ghost gap-1.5" onclick={() => playAllTracks(true)}>
								<Shuffle class="h-4 w-4" />
								Shuffle
							</button>
						{/if}
						{#if link.video_id}
							<button class="btn btn-sm btn-ghost gap-1.5" onclick={playFullAlbum}>
								<ExternalLink class="h-4 w-4" />
								Full Video
							</button>
						{/if}

						{#if tracks.length > 0}
							<ContextMenu items={getBulkMenuItems()} position="end" size="sm" />
						{/if}

						<div class="flex-1"></div>
						<button class="btn btn-sm btn-ghost gap-1" onclick={handleEdit}>
							<Pencil class="h-3.5 w-3.5" />
							Edit
						</button>
						{#if confirmingDelete}
							<button class="btn btn-sm btn-error gap-1" onclick={handleDelete} disabled={deleting}>
								{#if deleting}
									<span class="loading loading-spinner loading-xs"></span>
								{:else}
									Confirm
								{/if}
							</button>
							<button
								class="btn btn-sm btn-ghost"
								onclick={() => {
									confirmingDelete = false;
								}}>Cancel</button
							>
						{:else}
							<button class="btn btn-sm btn-ghost text-error gap-1" onclick={handleDelete}>
								<Trash2 class="h-3.5 w-3.5" />
							</button>
						{/if}
					</div>
				</div>

				<button class="btn btn-sm btn-circle btn-ghost self-start -mr-2 -mt-2" onclick={handleClose}
					><X class="h-4 w-4" /></button
				>
			</div>

			<div class="divider my-0 mx-6"></div>

			<div class="px-6 pt-3 pb-5 max-h-128 overflow-y-auto">
				{#if loadingTracks}
					<div class="flex justify-center py-8">
						<span class="loading loading-spinner loading-md"></span>
					</div>
				{:else if trackError}
					<div class="text-center py-10">
						<div class="alert alert-error max-w-sm mx-auto">
							<span class="text-sm">Couldn't load the track list</span>
							<button
								class="btn btn-sm btn-ghost"
								onclick={() => link && fetchTracks(link.album_id)}>Retry</button
							>
						</div>
					</div>
				{:else if tracks.length > 0}
					<div class="flex flex-col gap-0.5">
						{#each trackSections as section (section.discNumber)}
							{#if trackSections.length > 1}
								<div class="px-3 pt-3 pb-1">
									<div
										class="inline-flex items-center gap-2 rounded-full border border-base-content/10 bg-base-200 px-3 py-1 text-xs font-semibold uppercase tracking-[0.18em] opacity-70"
									>
										<span class="h-1.5 w-1.5 rounded-full bg-accent"></span>
										Disc {section.discNumber}
									</div>
								</div>
							{/if}
							{#each section.tracks as track (track.track_name + track.disc_number + track.track_number)}
								{@const isCurrentlyPlaying =
									playerStore.nowPlaying?.albumId === link.album_id &&
									(playerStore.currentQueueItem?.discNumber ?? 1) ===
										normalizeDiscNumber(track.disc_number) &&
									playerStore.currentQueueItem?.trackNumber === track.track_number &&
									playerStore.isPlaying}
								<div
									class={[
										'flex items-center gap-3 w-full py-2.5 px-3 rounded-lg transition-colors group/track',
										!isCurrentlyPlaying && 'hover:bg-base-200'
									]}
									style={isCurrentlyPlaying ? `background-color: ${colors.accent}15;` : ''}
								>
									<button
										class="flex items-center gap-3 flex-1 min-w-0 text-left cursor-pointer"
										onclick={() => playTrack(track)}
									>
										<span
											class="font-mono w-7 text-right text-sm shrink-0 {isCurrentlyPlaying
												? ''
												: 'opacity-40'}"
											style={isCurrentlyPlaying ? `color: ${colors.accent};` : ''}
										>
											{#if isCurrentlyPlaying}
												<NowPlayingIndicator />
											{:else}
												{track.track_number}
											{/if}
										</span>
										<span
											class="text-sm truncate flex-1"
											style={isCurrentlyPlaying ? `color: ${colors.accent};` : ''}
											>{track.track_name}</span
										>
										<Play
											class="h-4 w-4 shrink-0 transition-opacity {isCurrentlyPlaying
												? 'opacity-100'
												: 'opacity-0 group-hover/track:opacity-100'} fill-current"
											style={isCurrentlyPlaying
												? `color: ${colors.accent};`
												: `color: ${colors.accent};`}
										/>
									</button>

									<div class="shrink-0">
										<ContextMenu items={getTrackMenuItems(track)} position="end" size="xs" />
									</div>
								</div>
							{/each}
						{/each}
					</div>
				{:else if !link.video_id}
					<div class="text-center py-10">
						<YouTubeIcon class="h-10 w-10 mx-auto opacity-20 mb-2" />
						<p class="text-sm opacity-50">No tracks linked yet</p>
						<p class="text-xs opacity-30 mt-1">Add track links to enable per-track playback.</p>
					</div>
				{:else}
					<div class="text-center py-10">
						<Play class="h-10 w-10 mx-auto opacity-20 mb-2" />
						<p class="text-sm opacity-50">Album-level link only</p>
						<p class="text-xs opacity-30 mt-1">Individual tracks are not linked for this album.</p>
					</div>
				{/if}
			</div>
		</div>

		<form method="dialog" class="modal-backdrop">
			<button onclick={handleClose}>close</button>
		</form>
	</dialog>
{/if}
