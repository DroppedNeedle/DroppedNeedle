<script lang="ts">
	/*
	 * QueueDrawer — right-hand slide-over queue (used by the Listening Room).
	 * The queue body itself lives in player/QueueList.svelte, shared with the
	 * full-screen Stage.
	 */
	import { fly, fade } from 'svelte/transition';
	import { playerStore } from '$lib/stores/player.svelte';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import QueueList from '$lib/components/player/QueueList.svelte';
	import { X, ListMusic, Shuffle, Trash2 } from 'lucide-svelte';

	interface Props {
		open: boolean;
		onclose: () => void;
	}

	let { open = $bindable(), onclose }: Props = $props();

	function handleClose() {
		open = false;
		onclose();
	}

	function handleKeydown(e: KeyboardEvent) {
		if (!open) return;
		if (e.key === 'Escape') handleClose();
	}

	function handleClearQueue() {
		playerStore.clearQueue();
		playbackToast.show('Upcoming queue cleared', 'info');
		handleClose();
	}

	$effect(() => {
		if (open) {
			document.body.classList.add('overflow-hidden');
		} else {
			document.body.classList.remove('overflow-hidden');
		}
		return () => {
			document.body.classList.remove('overflow-hidden');
		};
	});

	const queue = $derived(playerStore.queue);
	const upcomingCount = $derived(playerStore.upcomingQueueLength);
</script>

<svelte:window onkeydown={handleKeydown} />

{#if open}
	<button
		class="fixed inset-0 z-[60] bg-black/40 backdrop-blur-sm"
		onclick={handleClose}
		aria-label="Close queue"
		transition:fade={{ duration: 200 }}
	></button>

	<div
		class="fixed right-0 top-0 bottom-0 z-[61] w-full max-w-md bg-base-200 shadow-2xl flex flex-col"
		transition:fly={{ x: 400, duration: 200 }}
	>
		<div class="flex items-center justify-between p-4 border-b border-base-content/10">
			<div class="flex items-center gap-2">
				<ListMusic class="h-5 w-5" />
				<h2 class="text-lg font-bold">Queue</h2>
				{#if queue.length > 0}
					<span class="badge badge-sm badge-neutral">{upcomingCount}</span>
				{/if}
			</div>
			<div class="flex items-center gap-1">
				{#if queue.length > 0}
					<button
						class="btn btn-ghost btn-sm btn-circle"
						class:text-accent={playerStore.shuffleEnabled}
						class:opacity-50={!playerStore.shuffleEnabled}
						onclick={() => playerStore.toggleShuffle()}
						aria-label="Toggle shuffle"
					>
						<Shuffle class="h-3.5 w-3.5" />
					</button>
					<button class="btn btn-ghost btn-sm gap-1 text-error" onclick={handleClearQueue}>
						<Trash2 class="h-3.5 w-3.5" />
						Clear
					</button>
				{/if}
				<button
					class="btn btn-ghost btn-sm btn-circle"
					onclick={handleClose}
					aria-label="Close queue"
				>
					<X class="h-4 w-4" />
				</button>
			</div>
		</div>

		<div class="flex-1 overflow-y-auto">
			<QueueList active={open} />
		</div>

		{#if queue.length > 0}
			<div class="p-3 border-t border-base-content/10 text-xs opacity-50 text-center">
				{upcomingCount} track{upcomingCount === 1 ? '' : 's'} upcoming
			</div>
		{/if}
	</div>
{/if}
