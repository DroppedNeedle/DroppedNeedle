<!--
	The drawer's Queue tab: the queue in play order, the playing track
	outlined, then "Up next" with the rows that can be dragged, moved with
	the arrow keys on their handle, or removed. Rows the rating floor will
	pass over are dimmed and marked Skips, and still play when picked. Clear
	empties what is upcoming and calls `oncleared`, which shuts the drawer.
-->
<script lang="ts">
	import { isValidMbid } from '$lib/utils/formatting';
	import { tick } from 'svelte';
	import {
		formatLabel,
		playerStore,
		type UnplayableReason
	} from '$lib/features/player/state/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { CircleSlash, GripVertical, ListMusic, Shuffle, Trash2 } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import NowPlayingIndicator from '$lib/features/player/components/NowPlayingIndicator.svelte';
	import type { QueueItem } from '$lib/features/player/playback/types';
	import { skipByRating, skippedRating } from '$lib/features/player/state/skipByRating.svelte';

	interface Props {
		/** Called after Clear, which ends the reason to keep the queue open. */
		oncleared: () => void;
	}

	let { oncleared }: Props = $props();

	let list = $state<HTMLElement | null>(null);
	let scroller = $state<HTMLElement | null>(null);
	let dragOverIndex = $state<number | null>(null);
	let dragSourceIndex = $state<number | null>(null);

	const sourceLabels: Record<QueueItem['sourceType'], string> = {
		local: 'Local',
		jellyfin: 'Jellyfin',
		navidrome: 'Navidrome',
		plex: 'Plex',
		youtube: 'YT'
	};

	const queue = $derived(playerStore.queue);
	const currentIndex = $derived(playerStore.currentIndex);
	const upcomingCount = $derived(playerStore.upcomingQueueLength);
	const floor = $derived(skipByRating.floor);
	const displayOrder = $derived(
		playerStore.shuffleEnabled ? playerStore.shuffleOrder : queue.map((_, i) => i)
	);
	const currentDisplayPosition = $derived.by(() => {
		if (playerStore.shuffleEnabled) {
			const pos = playerStore.shuffleOrder.indexOf(currentIndex);
			return pos >= 0 ? pos : currentIndex;
		}
		return currentIndex;
	});

	function removeTrack(index: number, e: Event) {
		e.stopPropagation();
		const trackName = queue[index]?.trackName ?? 'Track';
		playerStore.removeFromQueue(index);
		toastStore.notify(`Removed "${trackName}" from queue`, 'info');
	}

	function handleClearQueue() {
		playerStore.clearQueue();
		toastStore.notify('Upcoming queue cleared', 'info');
		oncleared();
	}

	function handleDragStart(e: DragEvent, displayPos: number) {
		if (displayPos <= currentDisplayPosition) {
			e.preventDefault();
			return;
		}
		dragSourceIndex = displayPos;
		if (e.dataTransfer) {
			e.dataTransfer.effectAllowed = 'move';
			e.dataTransfer.setData('text/plain', String(displayPos));
		}
	}

	function handleDragOver(e: DragEvent, displayPos: number) {
		if (
			displayPos <= currentDisplayPosition ||
			dragSourceIndex === null ||
			dragSourceIndex <= currentDisplayPosition
		) {
			return;
		}
		e.preventDefault();
		if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
		dragOverIndex = displayPos;
	}

	function handleDrop(e: DragEvent, toDisplayPos: number) {
		e.preventDefault();
		const fromDisplayPos = dragSourceIndex;
		dragOverIndex = null;
		dragSourceIndex = null;
		if (
			toDisplayPos <= currentDisplayPosition ||
			fromDisplayPos === null ||
			fromDisplayPos <= currentDisplayPosition ||
			fromDisplayPos === toDisplayPos
		) {
			return;
		}
		if (playerStore.shuffleEnabled) {
			playerStore.reorderShuffleOrder(fromDisplayPos, toDisplayPos);
		} else {
			playerStore.reorderQueue(displayOrder[fromDisplayPos], displayOrder[toDisplayPos]);
		}
	}

	function handleDragEnd() {
		dragOverIndex = null;
		dragSourceIndex = null;
	}

	function handleHandleKeydown(e: KeyboardEvent, displayPosition: number) {
		if (displayPosition <= currentDisplayPosition) return;
		let newPos: number | null = null;
		if (e.key === 'ArrowUp' && displayPosition > currentDisplayPosition + 1) {
			newPos = displayPosition - 1;
		} else if (e.key === 'ArrowDown' && displayPosition < displayOrder.length - 1) {
			newPos = displayPosition + 1;
		}
		if (newPos === null) return;
		e.preventDefault();
		e.stopPropagation();
		if (playerStore.shuffleEnabled) {
			playerStore.reorderShuffleOrder(displayPosition, newPos);
		} else {
			playerStore.reorderQueue(displayOrder[displayPosition], displayOrder[newPos]);
		}
		const moved = newPos;
		// Only upcoming rows have a handle, so the moved row's is that far past the playing one.
		tick().then(() => {
			const handles = list?.querySelectorAll<HTMLElement>('[aria-label="Drag to reorder"]');
			handles?.[moved - currentDisplayPosition - 1]?.focus();
		});
		toastStore.notify(`Track moved to position ${moved + 1}`, 'info');
	}

	function unplayableTitle(reason: UnplayableReason, format: string | undefined): string {
		if (reason === 'conversion_failed') return "Couldn't be converted for this browser";
		const why = reason === 'converting_off' ? 'transcoding is off' : "the server can't convert it";
		return `This browser can't play ${formatLabel(format)}, and ${why}`;
	}

	function formatDuration(seconds?: number): string {
		if (!seconds || isNaN(seconds)) return '';
		const m = Math.floor(seconds / 60);
		const s = Math.floor(seconds % 60);
		return `${m}:${s.toString().padStart(2, '0')}`;
	}

	// Centres the playing row in the list when the tab opens, scrolling the list alone.
	function revealIfCurrent(node: HTMLElement, isCurrent: boolean) {
		if (!isCurrent) return;
		queueMicrotask(() => {
			const box = scroller;
			if (!box) return;
			const offset = node.getBoundingClientRect().top - box.getBoundingClientRect().top;
			box.scrollTop += offset - box.clientHeight / 2 + node.offsetHeight / 2;
		});
	}
</script>

{#if queue.length === 0}
	<div class="flex flex-1 flex-col items-center justify-center gap-3 p-8 text-center">
		<ListMusic class="size-10 text-muted" aria-hidden="true" />
		<p class="text-sm font-semibold">Queue is empty</p>
		<p class="text-xs text-muted">Add tracks from album pages or your library</p>
	</div>
{:else}
	<div class="flex items-center gap-2 border-b border-line px-4 py-2.5">
		<StatusChip size="sm">{upcomingCount} upcoming</StatusChip>
		<span class="flex-1"></span>
		<IconButton
			size="sm"
			icon={Shuffle}
			label="Toggle shuffle"
			active={playerStore.shuffleEnabled}
			aria-pressed={playerStore.shuffleEnabled}
			onclick={() => playerStore.toggleShuffle()}
		/>
		<Button variant="danger-ghost" size="sm" icon={Trash2} onclick={handleClearQueue}>Clear</Button>
	</div>

	<div bind:this={scroller} class="min-h-0 flex-1 overflow-y-auto px-3 py-3">
		<div bind:this={list} class="flex flex-col gap-0.5" role="group" aria-label="Queue tracks">
			{#each displayOrder as queueIndex, displayPosition (queueIndex)}
				{@const item = queue[queueIndex]}
				{@const isCurrent = queueIndex === currentIndex}
				{@const isPlayed = displayPosition < currentDisplayPosition}
				{@const isReorderable = displayPosition > currentDisplayPosition}
				{@const unplayable = playerStore.unplayableReasonFor(item)}
				{@const skipsAt = isReorderable
					? skippedRating(floor, playerStore.floorRatings, item)
					: null}
				{#if displayPosition === currentDisplayPosition + 1}
					<p class="dn-queue-divider">Up next</p>
				{/if}
				<div
					use:revealIfCurrent={isCurrent}
					class="dn-queue-row group/item"
					class:dn-queue-row--current={isCurrent}
					class:dn-queue-row--played={isPlayed}
					class:dn-queue-row--skips={skipsAt !== null}
					class:dn-queue-row--drop={dragOverIndex === displayPosition}
					draggable={isReorderable}
					ondragstart={(e) => handleDragStart(e, displayPosition)}
					ondragover={(e) => handleDragOver(e, displayPosition)}
					ondragleave={() => (dragOverIndex = null)}
					ondrop={(e) => handleDrop(e, displayPosition)}
					ondragend={handleDragEnd}
					onclick={() => playerStore.jumpToTrack(queueIndex)}
					onkeydown={(e) => {
						if (e.target !== e.currentTarget) return;
						if (e.key === 'Enter' || e.key === ' ') {
							e.preventDefault();
							playerStore.jumpToTrack(queueIndex);
						}
					}}
					role="button"
					tabindex="0"
				>
					{#if isReorderable}
						<button
							type="button"
							class="dn-queue-handle"
							aria-label="Drag to reorder"
							onkeydown={(e) => handleHandleKeydown(e, displayPosition)}
							onclick={(e) => e.stopPropagation()}
						>
							<GripVertical class="size-4" aria-hidden="true" />
						</button>
					{:else}
						<span class="dn-queue-pos">{displayPosition + 1}</span>
					{/if}

					<AlbumImage
						mbid={isValidMbid(item.albumId) ? item.albumId : ''}
						customUrl={item.coverUrl ?? null}
						alt={item.albumName}
						size="full"
						requestSize={250}
						rounded="sm"
						className="size-10 shrink-0 {isPlayed
							? 'opacity-60'
							: skipsAt !== null
								? 'opacity-40'
								: ''}"
					/>

					<div class="min-w-0 flex-1">
						<p class="dn-queue-title truncate">{item.trackName || item.albumName}</p>
						<p class="dn-queue-artist truncate text-xs text-muted">{item.artistName}</p>
					</div>

					{#if isCurrent && playerStore.isPlaying}
						<NowPlayingIndicator size="md" color="bg-primary" />
					{/if}
					{#if skipsAt !== null}
						<span class="inline-flex" title={skipsAt > 0 ? `Rated ${skipsAt}★` : 'Not rated'}>
							<StatusChip size="xs">Skips</StatusChip>
						</span>
					{/if}
					{#if unplayable}
						<span class="inline-flex" title={unplayableTitle(unplayable, item.format)}>
							<StatusChip size="xs" icon={CircleSlash}>Can't play</StatusChip>
						</span>
					{/if}
					{#if item.duration}
						<span class="dn-queue-time">{formatDuration(item.duration)}</span>
					{/if}
					<StatusChip size="xs">{sourceLabels[item.sourceType]}</StatusChip>

					<span class="w-6 shrink-0">
						{#if !(isCurrent && playerStore.isPlaying)}
							<IconButton
								size="xs"
								variant="danger-ghost"
								icon={Trash2}
								label="Remove from queue"
								class="dn-queue-remove"
								onclick={(e: MouseEvent) => removeTrack(queueIndex, e)}
							/>
						{/if}
					</span>
				</div>
			{/each}
		</div>
	</div>

	<p class="border-t border-line px-4 py-3 text-center font-mono text-xs text-muted">
		{upcomingCount} track{upcomingCount === 1 ? '' : 's'} upcoming
	</p>
{/if}

<style>
	.dn-queue-divider {
		margin: 0.75rem 0.5rem 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.24em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-queue-row {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		min-height: 3.5rem;
		padding: 0.375rem 0.5rem;
		border: 2px solid transparent;
		border-radius: var(--radius-field);
		cursor: pointer;
		transition: background-color 120ms;
	}
	.dn-queue-row:hover {
		background: var(--dn-selected);
	}
	.dn-queue-row:focus-visible {
		outline-offset: -2px;
	}
	.dn-queue-row--current {
		border-color: var(--dn-title);
		background: var(--dn-selected);
	}
	.dn-queue-row--current .dn-queue-title {
		color: var(--dn-title);
	}
	.dn-queue-row--played .dn-queue-title {
		color: var(--dn-muted);
	}
	/* a track the rating floor will pass over */
	.dn-queue-row--skips .dn-queue-title,
	.dn-queue-row--skips .dn-queue-artist {
		opacity: 0.45;
	}
	.dn-queue-row--drop {
		box-shadow: inset 0 2px 0 var(--dn-title);
	}
	.dn-queue-title {
		font-size: 0.875rem;
		font-weight: 600;
	}
	.dn-queue-pos,
	.dn-queue-handle {
		display: grid;
		width: 1.25rem;
		flex-shrink: 0;
		place-items: center;
	}
	.dn-queue-pos {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		color: var(--dn-muted);
	}
	.dn-queue-handle {
		height: 2rem;
		border-radius: 0.25rem;
		color: var(--dn-muted);
		cursor: grab;
	}
	.dn-queue-handle:hover {
		color: var(--color-base-content);
	}
	.dn-queue-handle:active {
		cursor: grabbing;
	}
	.dn-queue-time {
		flex-shrink: 0;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-variant-numeric: tabular-nums;
		color: var(--dn-muted);
	}
	/* the remove button shows on hover and focus, and always on touch */
	.dn-queue-row :global(.dn-queue-remove) {
		opacity: 0;
	}
	.dn-queue-row:hover :global(.dn-queue-remove),
	.dn-queue-row :global(.dn-queue-remove:focus-visible) {
		opacity: 1;
	}
	@media (hover: none) {
		.dn-queue-row :global(.dn-queue-remove) {
			opacity: 1;
		}
	}
</style>
