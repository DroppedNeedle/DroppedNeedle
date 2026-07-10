<script lang="ts">
	/*
	 * QueueList — the queue body shared by the QueueDrawer and the Stage.
	 * Carries the full reorder contract: drag + keyboard reorder of upcoming
	 * tracks (shuffle-order aware), jump-to-track, remove, played dimming.
	 */
	import { tick } from 'svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { X, GripVertical, ListMusic, Disc3 } from 'lucide-svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import LocalFilesIcon from '$lib/components/LocalFilesIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';

	interface Props {
		/** scroll the current track into view when this flips true */
		active?: boolean;
	}

	let { active = true }: Props = $props();

	let dragOverIndex = $state<number | null>(null);
	let dragSourceIndex = $state<number | null>(null);
	let listEl: HTMLElement | undefined = $state();
	let currentTrackEl: HTMLElement | null = null;

	function trackCurrentEl(node: HTMLElement, isCurrent: boolean) {
		if (isCurrent) currentTrackEl = node;
		return {
			update(isCurrent: boolean) {
				if (isCurrent) currentTrackEl = node;
			},
			destroy() {
				if (currentTrackEl === node) currentTrackEl = null;
			}
		};
	}

	function jumpToTrack(index: number) {
		playerStore.jumpToTrack(index);
	}

	function removeTrack(index: number, e: Event) {
		e.stopPropagation();
		const trackName = queue[index]?.trackName ?? 'Track';
		playerStore.removeFromQueue(index);
		playbackToast.show(`Removed "${trackName}" from queue`, 'info');
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

	function handleDragLeave() {
		dragOverIndex = null;
	}

	function handleDrop(e: DragEvent, toDisplayPos: number) {
		e.preventDefault();
		const fromDisplayPos = dragSourceIndex;
		dragOverIndex = null;
		dragSourceIndex = null;
		if (
			toDisplayPos <= currentDisplayPosition ||
			fromDisplayPos === null ||
			fromDisplayPos <= currentDisplayPosition
		) {
			return;
		}
		if (fromDisplayPos !== toDisplayPos) {
			if (playerStore.shuffleEnabled) {
				playerStore.reorderShuffleOrder(fromDisplayPos, toDisplayPos);
			} else {
				playerStore.reorderQueue(displayOrder[fromDisplayPos], displayOrder[toDisplayPos]);
			}
		}
	}

	function handleDragEnd() {
		dragOverIndex = null;
		dragSourceIndex = null;
	}

	function handleItemKeydown(e: KeyboardEvent, displayPosition: number) {
		if (displayPosition <= currentDisplayPosition) return;
		let newPos: number | null = null;
		if (e.key === 'ArrowUp' && displayPosition > currentDisplayPosition + 1) {
			e.preventDefault();
			e.stopPropagation();
			newPos = displayPosition - 1;
		} else if (e.key === 'ArrowDown' && displayPosition < displayOrder.length - 1) {
			e.preventDefault();
			e.stopPropagation();
			newPos = displayPosition + 1;
		}
		if (newPos === null) return;
		if (playerStore.shuffleEnabled) {
			playerStore.reorderShuffleOrder(displayPosition, newPos);
		} else {
			playerStore.reorderQueue(displayOrder[displayPosition], displayOrder[newPos]);
		}
		tick().then(() => {
			const handles = listEl?.querySelectorAll<HTMLElement>('[aria-label="Drag to reorder"]');
			handles?.[newPos!]?.focus();
		});
		playbackToast.show(`Track moved to position ${newPos + 1}`, 'info');
	}

	function formatDuration(seconds?: number): string {
		if (!seconds || isNaN(seconds)) return '';
		const m = Math.floor(seconds / 60);
		const s = Math.floor(seconds % 60);
		return `${m}:${s.toString().padStart(2, '0')}`;
	}

	$effect(() => {
		if (active && currentTrackEl) {
			queueMicrotask(() => {
				currentTrackEl?.scrollIntoView({ block: 'center', behavior: 'smooth' });
			});
		}
	});

	const queue = $derived(playerStore.queue);
	const currentIndex = $derived(playerStore.currentIndex);
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
</script>

{#if queue.length === 0}
	<div class="flex flex-col items-center justify-center h-full gap-3 p-8">
		<ListMusic class="h-12 w-12 opacity-20" />
		<p class="text-sm opacity-50">Queue is empty</p>
		<p class="text-xs opacity-30 text-center">Add tracks from album pages or your library</p>
	</div>
{:else}
	<div class="flex flex-col" role="group" aria-label="Queue tracks" bind:this={listEl}>
		{#each displayOrder as queueIndex, displayPosition (queueIndex)}
			{@const item = queue[queueIndex]}
			{@const isCurrent = queueIndex === currentIndex}
			{@const isPlayed = displayPosition < currentDisplayPosition}
			{@const isReorderable = displayPosition > currentDisplayPosition}
			{@const coverUrl = getCoverUrl(item.coverUrl, item.albumId)}
			{#if displayPosition === currentDisplayPosition + 1}
				<div class="flex items-center gap-2 px-4 py-1.5">
					<span class="text-[0.65rem] font-semibold uppercase tracking-wider opacity-40"
						>Up next</span
					>
					<div class="flex-1 border-t border-base-content/5"></div>
				</div>
			{/if}
			<div
				use:trackCurrentEl={isCurrent}
				class="flex items-center gap-2 px-3 py-2 transition-colors cursor-pointer group/item
					{isCurrent
					? 'bg-accent/10 border-l-2 border-accent'
					: 'hover:bg-base-content/5 border-l-2 border-transparent'}
					{isPlayed ? 'opacity-40' : ''}
					{dragOverIndex === displayPosition ? 'border-t-2 border-t-accent' : ''}"
				draggable={isReorderable}
				ondragstart={(e) => handleDragStart(e, displayPosition)}
				ondragover={(e) => handleDragOver(e, displayPosition)}
				ondragleave={handleDragLeave}
				ondrop={(e) => handleDrop(e, displayPosition)}
				ondragend={handleDragEnd}
				onclick={() => jumpToTrack(queueIndex)}
				onkeydown={(e) => {
					if (e.key === 'Enter' || e.key === ' ') {
						e.preventDefault();
						jumpToTrack(queueIndex);
					}
				}}
				role="button"
				tabindex="0"
			>
				{#if isReorderable}
					<button
						class="shrink-0 opacity-40 group-hover/item:opacity-70 focus:opacity-80 cursor-grab active:cursor-grabbing transition-opacity bg-transparent border-none p-0"
						aria-label="Drag to reorder"
						onkeydown={(e) => handleItemKeydown(e, displayPosition)}
						onclick={(e) => e.stopPropagation()}
						tabindex="0"
					>
						<GripVertical class="h-4 w-4" />
					</button>
				{:else}
					<div class="shrink-0 w-4 h-4"></div>
				{/if}

				<div class="shrink-0 w-10 h-10 rounded overflow-hidden">
					{#if coverUrl}
						<img src={coverUrl} alt={item.albumName} class="w-full h-full object-cover" />
					{:else}
						<div class="w-full h-full bg-base-200 flex items-center justify-center">
							<Disc3 class="h-4 w-4 text-base-content/20" />
						</div>
					{/if}
				</div>

				<div class="flex-1 min-w-0">
					<p class="text-sm font-medium truncate {isCurrent ? 'text-accent' : ''}">
						{item.trackName || item.albumName}
					</p>
					<p class="text-xs opacity-60 truncate">{item.artistName}</p>
				</div>

				{#if item.duration}
					<span class="text-xs opacity-40 shrink-0">{formatDuration(item.duration)}</span>
				{/if}

				<div class="shrink-0">
					{#if item.sourceType === 'jellyfin'}
						<span title="Jellyfin" style="color: rgb(var(--brand-jellyfin));">
							<JellyfinIcon class="h-3.5 w-3.5" />
						</span>
					{:else if item.sourceType === 'navidrome'}
						<span title="Navidrome" style="color: rgb(var(--brand-navidrome));">
							<NavidromeIcon class="h-3.5 w-3.5" />
						</span>
					{:else if item.sourceType === 'local'}
						<span title="Local" style="color: rgb(var(--brand-localfiles));">
							<LocalFilesIcon class="h-3.5 w-3.5" />
						</span>
					{:else if item.sourceType === 'youtube'}
						<span class="badge badge-xs badge-ghost">YT</span>
					{:else if item.sourceType === 'plex'}
						<span title="Plex" style="color: rgb(var(--brand-plex));">
							<PlexIcon class="h-3.5 w-3.5" />
						</span>
					{/if}
				</div>

				<div class="shrink-0 w-6">
					{#if isCurrent && playerStore.isPlaying}
						<NowPlayingIndicator />
					{:else}
						<button
							class="btn btn-ghost btn-xs btn-circle opacity-0 group-hover/item:opacity-60 transition-opacity"
							onclick={(e) => removeTrack(queueIndex, e)}
							aria-label="Remove from queue"
						>
							<X class="h-3.5 w-3.5" />
						</button>
					{/if}
				</div>
			</div>
		{/each}
	</div>
{/if}
