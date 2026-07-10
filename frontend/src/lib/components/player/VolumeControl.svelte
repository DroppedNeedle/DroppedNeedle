<script lang="ts">
	/*
	 * VolumeControl — volume slider with a click-to-mute icon.
	 */
	import { playerStore } from '$lib/stores/player.svelte';
	import { Volume2, Volume1, VolumeX } from 'lucide-svelte';

	interface Props {
		width?: string;
	}

	let { width = 'w-20' }: Props = $props();

	let lastNonZero = 70;

	function handleVolume(e: Event): void {
		const target = e.target as HTMLInputElement;
		playerStore.setVolume(Number(target.value));
	}

	function toggleMute(): void {
		if (playerStore.volume > 0) {
			lastNonZero = playerStore.volume;
			playerStore.setVolume(0);
		} else {
			playerStore.setVolume(lastNonZero);
		}
	}
</script>

<div class="flex items-center gap-1.5">
	<button
		class="btn btn-ghost btn-xs btn-circle opacity-60 hover:opacity-100"
		onclick={toggleMute}
		aria-label={playerStore.volume === 0 ? 'Unmute' : 'Mute'}
	>
		{#if playerStore.volume === 0}
			<VolumeX class="h-4 w-4" />
		{:else if playerStore.volume < 50}
			<Volume1 class="h-4 w-4" />
		{:else}
			<Volume2 class="h-4 w-4" />
		{/if}
	</button>
	<input
		type="range"
		class="range range-xs {width}"
		min="0"
		max="100"
		value={playerStore.volume}
		oninput={handleVolume}
		aria-label="Volume"
	/>
</div>
