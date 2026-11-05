<script lang="ts">
	import { tick } from 'svelte';
	import { slide, fly } from '$lib/ui/motion';
	import type { PlaylistDetail, PlaylistTrack } from '$lib/api/playlists';
	import {
		createRemovePlaylistTrackV3Mutation,
		createRemovePlaylistTracksV3Mutation,
		createReorderPlaylistTrackV3Mutation,
		createUpdatePlaylistTrackV3Mutation
	} from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import { toPageTrack } from '$lib/features/playlists/queries/playlistV3Adapter';
	import { playlistTrackToQueueItem } from '$lib/features/player/playback/queueHelpers';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { SourceType } from '$lib/features/player/playback/types';
	import { formatDurationSec, isValidMbid } from '$lib/utils/formatting';
	import { withBasePath } from '$lib/utils/basePath';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ContextMenu from '$lib/ui/ContextMenu.svelte';
	import type { MenuItem } from '$lib/ui/ContextMenu.svelte';
	import SourcePickerDropdown from '$lib/features/playlists/components/SourcePickerDropdown.svelte';
	import NowPlayingIndicator from '$lib/features/player/components/NowPlayingIndicator.svelte';
	import { Trash2, ListPlus, ListStart, GripVertical, Play, X } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import VinylRecord from '$lib/ui/VinylRecord.svelte';
	import PaperPanel from '$lib/ui/PaperPanel.svelte';
	import { SvelteSet } from 'svelte/reactivity';

	interface Props {
		playlist: PlaylistDetail;
		ontrackchange: () => void;
		onsourcechange?: () => void;
		onplaytrack?: (index: number) => void;
		readonly?: boolean;
	}

	let { playlist, ontrackchange, onsourcechange, onplaytrack, readonly = false }: Props = $props();

	let dragIndex = $state<number | null>(null);
	let dragOverIndex = $state<number | null>(null);
	let removingTrackIds = $state<Set<string>>(new Set());
	let liveMessage = $state('');

	let reorderTimeout: ReturnType<typeof setTimeout> | null = null;
	let pendingReorderTrackId: string | null = null;
	let pendingReorderPosition: number | null = null;
	let preKeyboardTracks: PlaylistTrack[] | null = null;

	let selectedIds = new SvelteSet<string>();
	let lastClickedIndex = $state<number | null>(null);
	let bulkRemoving = $state(false);
	let selectionMode = $derived(selectedIds.size > 0);

	const removeTrackMutation = createRemovePlaylistTrackV3Mutation();
	const removeTracksMutation = createRemovePlaylistTracksV3Mutation();
	const updateTrackMutation = createUpdatePlaylistTrackV3Mutation();
	const reorderTrackMutation = createReorderPlaylistTrackV3Mutation();

	function toggleTrackSelection(trackId: string, index: number, shiftKey: boolean) {
		if (shiftKey && lastClickedIndex !== null) {
			const from = Math.min(lastClickedIndex, index);
			const to = Math.max(lastClickedIndex, index);
			for (let j = from; j <= to; j++) {
				selectedIds.add(playlist.tracks[j].id);
			}
		} else if (selectedIds.has(trackId)) {
			selectedIds.delete(trackId);
		} else {
			selectedIds.add(trackId);
		}
		lastClickedIndex = index;
	}

	function toggleSelectAll() {
		if (selectedIds.size === playlist.tracks.length) {
			selectedIds.clear();
		} else {
			playlist.tracks.forEach((t) => selectedIds.add(t.id));
		}
	}

	function clearSelection() {
		selectedIds.clear();
		lastClickedIndex = null;
	}

	async function removeSelectedTracks() {
		if (bulkRemoving || selectedIds.size === 0) return;
		const ids = [...selectedIds];
		const count = ids.length;
		const prevTracks = [...playlist.tracks];
		const prevDuration = playlist.total_duration;
		const prevCount = playlist.track_count;

		bulkRemoving = true;
		const removedDuration = prevTracks
			.filter((t) => selectedIds.has(t.id))
			.reduce((sum, t) => sum + (t.duration ?? 0), 0);
		playlist.tracks = playlist.tracks.filter((t) => !selectedIds.has(t.id));
		playlist.track_count = playlist.tracks.length;
		playlist.total_duration = Math.max(0, (playlist.total_duration ?? 0) - removedDuration);
		clearSelection();

		try {
			await removeTracksMutation.mutateAsync({ id: playlist.id, trackIds: ids });
			toastStore.show({ message: `Removed ${count} track${count === 1 ? '' : 's'}`, type: 'info' });
			liveMessage = `${count} track${count === 1 ? '' : 's'} removed from playlist`;
			ontrackchange();
		} catch {
			playlist.tracks = prevTracks;
			playlist.track_count = prevCount;
			playlist.total_duration = prevDuration;
			toastStore.show({ message: "Couldn't remove those tracks", type: 'error' });
		} finally {
			bulkRemoving = false;
		}
	}

	function handleWindowKeydown(e: KeyboardEvent) {
		if (!selectionMode) return;
		const tag = (e.target as HTMLElement)?.tagName;
		if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
		if (e.key === 'Delete' || e.key === 'Backspace') {
			e.preventDefault();
			void removeSelectedTracks();
		}
		if (e.key === 'Escape') {
			e.preventDefault();
			clearSelection();
		}
	}

	async function removeTrack(track: PlaylistTrack) {
		if (removingTrackIds.has(track.id)) return;
		const prevTracks = [...playlist.tracks];
		const prevDuration = playlist.total_duration;
		removingTrackIds.add(track.id);
		playlist.tracks = playlist.tracks.filter((t) => t.id !== track.id);
		playlist.track_count = playlist.tracks.length;
		playlist.total_duration = Math.max(0, (playlist.total_duration ?? 0) - (track.duration ?? 0));
		try {
			await removeTrackMutation.mutateAsync({ id: playlist.id, trackId: track.id });
			toastStore.show({ message: `Removed "${track.track_name}"`, type: 'info' });
			liveMessage = `${track.track_name} removed from playlist`;
			ontrackchange();
		} catch {
			playlist.tracks = prevTracks;
			playlist.track_count = prevTracks.length;
			playlist.total_duration = prevDuration;
			toastStore.show({ message: "Couldn't remove that track", type: 'error' });
		} finally {
			removingTrackIds.delete(track.id);
		}
	}

	function addTrackToQueue(track: PlaylistTrack) {
		const item = playlistTrackToQueueItem(track);
		if (!item) {
			toastStore.show({ message: 'Track is not playable', type: 'error' });
			return;
		}
		playerStore.addToQueue(item);
	}

	function playTrackNext(track: PlaylistTrack) {
		const item = playlistTrackToQueueItem(track);
		if (!item) {
			toastStore.show({ message: 'Track is not playable', type: 'error' });
			return;
		}
		playerStore.playNext(item);
	}

	async function handleSourceChange(track: PlaylistTrack, newSourceType: string) {
		const prevSource = track.source_type;
		const prevSourceId = track.track_source_id;
		const prevFormat = track.format;
		track.source_type = newSourceType;
		try {
			const retargeted = await updateTrackMutation.mutateAsync({
				id: playlist.id,
				trackId: track.id,
				sourceType: newSourceType
			});
			const updated = toPageTrack(retargeted);
			track.track_source_id = updated.track_source_id;
			track.source_type = updated.source_type;
			track.format = updated.format;
			track.recording_mbid = updated.recording_mbid;
			if (updated.available_sources) {
				track.available_sources = updated.available_sources;
			}
			if (updated.track_source_id) {
				playerStore.updateQueueItemByPlaylistTrackId(
					track.id,
					updated.source_type as SourceType,
					updated.track_source_id,
					updated.format ?? undefined,
					updated.plex_rating_key ?? undefined
				);
			}
			onsourcechange?.();
		} catch {
			track.source_type = prevSource;
			track.track_source_id = prevSourceId;
			track.format = prevFormat;
			toastStore.show({ message: "Couldn't switch the source", type: 'error' });
		}
	}

	function handleDragStart(e: DragEvent, index: number) {
		if (e.dataTransfer) {
			e.dataTransfer.effectAllowed = 'move';
			e.dataTransfer.setData('text/plain', String(index));
		}
		dragIndex = index;
	}

	function handleDragOver(e: DragEvent, index: number) {
		e.preventDefault();
		dragOverIndex = index;
	}

	function handleDragLeave() {
		dragOverIndex = null;
	}

	async function handleDrop(e: DragEvent, toIndex: number) {
		e.preventDefault();
		if (dragIndex === null || dragIndex === toIndex) {
			dragIndex = null;
			dragOverIndex = null;
			return;
		}

		const fromIndex = dragIndex;
		const track = playlist.tracks[fromIndex];
		const prevTracks = [...playlist.tracks];

		const newTracks = [...playlist.tracks];
		newTracks.splice(fromIndex, 1);
		newTracks.splice(toIndex, 0, track);
		playlist.tracks = newTracks;

		dragIndex = null;
		dragOverIndex = null;

		try {
			await reorderTrackMutation.mutateAsync({
				id: playlist.id,
				trackId: track.id,
				newPosition: toIndex
			});
			liveMessage = `Track moved to position ${toIndex + 1}`;
		} catch {
			playlist.tracks = prevTracks;
			toastStore.show({ message: "Couldn't reorder that track", type: 'error' });
		}
	}

	function handleDragEnd() {
		dragIndex = null;
		dragOverIndex = null;
	}

	async function handleTrackKeydown(e: KeyboardEvent, index: number) {
		let newIndex: number | null = null;
		if (e.key === 'ArrowUp' && index > 0) {
			e.preventDefault();
			newIndex = index - 1;
		} else if (e.key === 'ArrowDown' && index < playlist.tracks.length - 1) {
			e.preventDefault();
			newIndex = index + 1;
		}
		if (newIndex === null) return;

		const track = playlist.tracks[index];

		if (preKeyboardTracks === null) {
			preKeyboardTracks = [...playlist.tracks];
		}

		const newTracks = [...playlist.tracks];
		newTracks.splice(index, 1);
		newTracks.splice(newIndex, 0, track);
		playlist.tracks = newTracks;

		tick().then(() => {
			const handles = document.querySelectorAll<HTMLElement>('[aria-label="Drag to reorder"]');
			handles[newIndex!]?.focus();
		});

		liveMessage = `Track moved to position ${newIndex + 1}`;
		pendingReorderTrackId = track.id;
		pendingReorderPosition = newIndex;

		if (reorderTimeout) clearTimeout(reorderTimeout);
		reorderTimeout = setTimeout(async () => {
			const savedTracks = preKeyboardTracks;
			const trackId = pendingReorderTrackId!;
			const position = pendingReorderPosition!;
			preKeyboardTracks = null;
			pendingReorderTrackId = null;
			pendingReorderPosition = null;
			reorderTimeout = null;
			try {
				await reorderTrackMutation.mutateAsync({
					id: playlist.id,
					trackId,
					newPosition: position
				});
			} catch {
				if (savedTracks) playlist.tracks = savedTracks;
				toastStore.show({ message: "Couldn't reorder that track", type: 'error' });
			}
		}, 400);
	}

	function getTrackMenuItems(track: PlaylistTrack): MenuItem[] {
		const items: MenuItem[] = [
			{
				label: 'Add to Queue',
				icon: ListPlus,
				onclick: () => addTrackToQueue(track)
			},
			{
				label: 'Play Next',
				icon: ListStart,
				onclick: () => playTrackNext(track)
			}
		];
		// Non-owners (public read-only view) cannot mutate the playlist.
		if (!readonly) {
			items.push({
				label: 'Remove',
				icon: Trash2,
				onclick: () => void removeTrack(track)
			});
		}
		return items;
	}

	export function clearReorderState() {
		if (reorderTimeout) {
			clearTimeout(reorderTimeout);
			reorderTimeout = null;
		}
		preKeyboardTracks = null;
		pendingReorderTrackId = null;
		pendingReorderPosition = null;
	}

	export function clearSelectionState() {
		clearSelection();
	}
</script>

<svelte:window onkeydown={handleWindowKeydown} />

{#if playlist.tracks.length === 0}
	<PaperPanel>
		<div class="flex flex-col items-center gap-2 px-6 pt-8 pb-12 text-center">
			<VinylRecord label class="mb-3 w-36" />
			<h2 class="text-2xl text-paper-content sm:text-3xl">This playlist is empty</h2>
			<p class="text-sm text-muted">Add tracks from album pages using the context menu</p>
		</div>
	</PaperPanel>
{:else}
	<PaperPanel>
		{#snippet head()}
			<div class="pl-row pl-row--head">
				{#if !readonly}
					<span class="pl-row__grip" aria-hidden="true"></span>
					<span class="pl-row__check">
						{#if playlist.tracks.length > 1}
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								title="Select all"
								aria-label="Select all"
								checked={selectedIds.size === playlist.tracks.length}
								indeterminate={selectedIds.size > 0 && selectedIds.size < playlist.tracks.length}
								onchange={toggleSelectAll}
							/>
						{/if}
					</span>
				{/if}
				<span class="pl-row__num">#</span>
				<span class="pl-row__cover" aria-hidden="true"></span>
				<span class="pl-row__title">Title</span>
				<span class="pl-row__album">Album</span>
				<span class="pl-row__time">Duration</span>
				<span class="pl-row__menu" aria-hidden="true"></span>
			</div>
		{/snippet}
		<ul class="divide-y divide-paper-border">
			{#each playlist.tracks as track, i (track.id)}
				{@const isCurrentlyPlaying =
					playerStore.isPlaying &&
					(playerStore.currentQueueItem?.playlistTrackId
						? playerStore.currentQueueItem.playlistTrackId === track.id
						: playerStore.currentQueueItem?.trackSourceId === track.track_source_id &&
							playerStore.currentQueueItem?.sourceType === track.source_type)}
				{@const albumLinked = isValidMbid(track.album_id)}
				<li
					transition:slide={{ duration: 200 }}
					class="pl-row group {selectedIds.has(track.id)
						? 'pl-row--selected'
						: isCurrentlyPlaying
							? 'pl-row--playing'
							: ''}"
					class:opacity-50={dragIndex === i}
					class:pl-row--drop={dragOverIndex === i && dragIndex !== null && dragIndex !== i}
					draggable={!selectionMode && !readonly}
					ondragstart={(e) => handleDragStart(e, i)}
					ondragover={(e) => handleDragOver(e, i)}
					ondragleave={handleDragLeave}
					ondrop={(e) => void handleDrop(e, i)}
					ondragend={handleDragEnd}
					role="listitem"
					aria-roledescription="sortable"
				>
					{#if !readonly}
						<button
							type="button"
							class="pl-row__grip cursor-grab touch-none transition-opacity active:cursor-grabbing {selectionMode
								? 'pointer-events-none opacity-0'
								: 'pl-reveal'}"
							aria-label="Drag to reorder"
							onkeydown={(e) => void handleTrackKeydown(e, i)}
							tabindex={selectionMode ? -1 : 0}
							aria-hidden={selectionMode ? 'true' : undefined}
						>
							<GripVertical class="size-4" />
						</button>

						<span class="pl-row__check">
							<input
								type="checkbox"
								class="checkbox checkbox-sm transition-opacity {selectionMode ? '' : 'pl-reveal'}"
								checked={selectedIds.has(track.id)}
								onclick={(e: MouseEvent) => {
									e.stopPropagation();
									toggleTrackSelection(track.id, i, e.shiftKey);
								}}
								aria-label="Select {track.track_name}"
							/>
						</span>
					{/if}

					<span class="pl-row__num">
						{#if isCurrentlyPlaying}
							<NowPlayingIndicator color="bg-primary" />
						{:else}
							<span class="pl-row__index">{i + 1}</span>
							<button
								type="button"
								class="pl-row__play"
								aria-label="Play {track.track_name}"
								onclick={(e) => {
									e.stopPropagation();
									onplaytrack?.(i);
								}}
							>
								<Play class="size-4 fill-current" />
							</button>
						{/if}
					</span>

					<AlbumImage
						customUrl={track.cover_url ?? null}
						alt=""
						size="full"
						requestSize={250}
						rounded="sm"
						className="pl-row__art size-10 shrink-0"
					/>

					<span class="pl-row__title">
						{#if albumLinked}
							<a href={withBasePath(`/album/${track.album_id}`)} class="pl-row__name"
								>{track.track_name}</a
							>
						{:else}
							<span class="pl-row__name">{track.track_name}</span>
						{/if}
						<span class="pl-row__sub">
							{#if track.artist_id}
								<a href={withBasePath(`/artist/${track.artist_id}`)} class="hover:underline"
									>{track.artist_name}</a
								>
							{:else}
								{track.artist_name}
							{/if}
							{#if track.album_name}
								<span class="md:hidden">
									<span aria-hidden="true"> · </span>
									{#if albumLinked}
										<a href={withBasePath(`/album/${track.album_id}`)} class="hover:underline"
											>{track.album_name}</a
										>
									{:else}
										{track.album_name}
									{/if}
								</span>
							{/if}
						</span>
					</span>

					{#if !readonly}
						<span class="pl-row__source">
							<SourcePickerDropdown
								currentSource={track.source_type}
								availableSources={track.available_sources ?? [track.source_type]}
								onchange={(src) => void handleSourceChange(track, src)}
							/>
						</span>
					{/if}

					<span class="pl-row__album">
						{#if track.album_name}
							{#if albumLinked}
								<a href={withBasePath(`/album/${track.album_id}`)} class="hover:underline"
									>{track.album_name}</a
								>
							{:else}
								{track.album_name}
							{/if}
						{/if}
					</span>

					<span class="pl-row__time">{formatDurationSec(track.duration)}</span>

					<span class="pl-row__menu">
						<ContextMenu
							items={getTrackMenuItems(track)}
							position="end"
							size="xs"
							label="More actions for {track.track_name}"
						/>
					</span>
				</li>
			{/each}
		</ul>
	</PaperPanel>

	{#if selectionMode}
		<div
			class="pl-selection"
			style:bottom="calc(var(--ms-bottom-nav-offset) + {playerStore.isPlayerVisible &&
			playerStore.nowPlaying
				? 'var(--ms-player-height) + '
				: ''}0.75rem)"
			transition:fly={{ y: 40, duration: 200 }}
		>
			<span class="pl-selection__count">{selectedIds.size} selected</span>
			<Button variant="ghost" size="sm" icon={X} onclick={clearSelection}>Deselect</Button>
			<Button
				variant="danger"
				size="sm"
				icon={Trash2}
				loading={bulkRemoving}
				onclick={() => void removeSelectedTracks()}
			>
				Delete
			</Button>
		</div>
	{/if}
{/if}

<div class="sr-only" aria-live="polite" role="status">{liveMessage}</div>

<style>
	.pl-row {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		min-height: 3.75rem;
		padding: 0.5rem 0.75rem;
		transition: background-color 150ms;
	}
	.pl-row:not(.pl-row--head):hover {
		background-color: color-mix(in oklab, var(--dn-paper-content) 5%, transparent);
	}
	.pl-row--playing,
	.pl-row--playing:not(.pl-row--head):hover {
		background-color: var(--dn-selected);
	}
	.pl-row--selected,
	.pl-row--selected:not(.pl-row--head):hover {
		background-color: color-mix(in oklab, var(--color-primary) 12%, transparent);
	}
	.pl-row--drop {
		box-shadow: inset 0 2px 0 var(--color-primary);
	}

	.pl-row--head {
		min-height: 2.75rem;
		padding-block: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	.pl-row__grip {
		display: grid;
		flex: none;
		place-items: center;
		width: 1.25rem;
		color: color-mix(in oklab, var(--dn-paper-content) 50%, transparent);
	}
	.pl-row__check {
		display: grid;
		flex: none;
		place-items: center;
		width: 1.25rem;
	}
	/* the owner's handle and tick show on a pointed or focused row; a touch
	   screen, with no hover, always shows them */
	@media (hover: hover) {
		.pl-row :global(.pl-reveal) {
			opacity: 0;
		}
		.pl-row:hover :global(.pl-reveal),
		.pl-row :global(.pl-reveal:focus-visible) {
			opacity: 1;
		}
	}

	.pl-row__num {
		position: relative;
		display: grid;
		flex: none;
		place-items: center;
		width: 1.75rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-variant-numeric: tabular-nums;
		color: var(--dn-muted);
	}
	.pl-row--head .pl-row__num {
		font-size: 0.6875rem;
	}
	/* the row's play sits over its number and shows on hover or focus */
	.pl-row__play {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		border-radius: 0.25rem;
		color: var(--color-primary);
		opacity: 0;
	}
	.pl-row__play:focus-visible,
	.pl-row:hover .pl-row__play {
		opacity: 1;
	}
	.pl-row:hover .pl-row__index,
	.pl-row:has(.pl-row__play:focus-visible) .pl-row__index {
		opacity: 0;
	}

	.pl-row--head .pl-row__cover {
		width: 2.5rem;
		flex: none;
	}

	.pl-row__title {
		display: flex;
		min-width: 0;
		flex: 1 1 0;
		flex-direction: column;
		gap: 0.125rem;
	}
	.pl-row__name {
		overflow: hidden;
		font-size: 0.9375rem;
		font-weight: 650;
		line-height: 1.3;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	a.pl-row__name:hover {
		text-decoration: underline;
	}
	.pl-row--playing .pl-row__name {
		color: var(--color-primary);
	}
	.pl-row__sub {
		overflow: hidden;
		font-size: 0.8125rem;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--dn-muted);
	}

	.pl-row__album {
		display: none;
		min-width: 0;
		flex: 0 1 28%;
		overflow: hidden;
		font-size: 0.875rem;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--dn-muted);
	}
	@media (min-width: 768px) {
		.pl-row__album {
			display: block;
		}
	}

	/* the source column keeps one width, so the album column lines up */
	.pl-row__source {
		display: flex;
		flex: none;
		justify-content: flex-end;
		min-width: 6.5rem;
	}

	.pl-row__time {
		flex: none;
		width: 4.25rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-variant-numeric: tabular-nums;
		text-align: right;
	}
	.pl-row--head :is(.pl-row__time, .pl-row__album) {
		font-size: 0.6875rem;
		color: var(--dn-muted);
	}

	.pl-row__menu {
		display: grid;
		flex: none;
		place-items: center;
		width: 1.75rem;
	}

	/* A phone keeps the number, the title, the duration and the menu: the
	   cover, the handle (touch screens cannot drag) and a source with
	   nothing to choose give the title their room. */
	@media (max-width: 639px) {
		.pl-row :global(.pl-row__art),
		.pl-row--head .pl-row__cover,
		.pl-row__grip {
			display: none;
		}
		.pl-row__source:not(:has(:global(.dropdown))) {
			display: none;
		}
		/* the picker keeps its icon; its name stays for screen readers */
		.pl-row__source :global(.dropdown > button > span) {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip-path: inset(50%);
			white-space: nowrap;
		}
	}

	@media (max-width: 479px) {
		.pl-row {
			gap: 0.5rem;
			padding-inline: 0.5rem;
		}
		.pl-row__time {
			width: auto;
		}
		.pl-row__source {
			min-width: 0;
		}
		.pl-row--head .pl-row__time {
			width: 3rem;
		}
	}

	/* the selection bar floats over the page, above the player and phone bar */
	.pl-selection {
		position: fixed;
		left: 50%;
		z-index: 50;
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.625rem 0.75rem 0.625rem 1.125rem;
		translate: -50% 0;
		border: 1px solid var(--dn-edge);
		border-radius: var(--radius-box);
		background-color: var(--color-base-200);
		box-shadow: 0 18px 40px -16px rgb(0 0 0 / 0.65);
		white-space: nowrap;
	}
	.pl-selection__count {
		margin-right: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}
</style>
