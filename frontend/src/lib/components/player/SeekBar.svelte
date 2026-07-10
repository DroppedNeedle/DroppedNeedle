<script lang="ts">
	/*
	 * SeekBar — elapsed / range / total. Disabled (with optional hint) for
	 * non-seekable streams, mirroring the previous player bar's contract.
	 */
	import { playerStore } from '$lib/stores/player.svelte';

	interface Props {
		showHint?: boolean;
		size?: 'dock' | 'stage';
	}

	let { showHint = false, size = 'dock' }: Props = $props();

	function formatTime(seconds: number): string {
		if (!seconds || isNaN(seconds)) return '0:00';
		const mins = Math.floor(seconds / 60);
		const secs = Math.floor(seconds % 60);
		return `${mins}:${secs.toString().padStart(2, '0')}`;
	}

	function handleSeek(e: Event): void {
		const target = e.target as HTMLInputElement;
		playerStore.seekTo(Number(target.value));
	}
</script>

<div class="flex w-full flex-col items-center gap-0.5">
	<div class="flex items-center gap-2 w-full">
		<span class="{size === 'stage' ? 'text-sm' : 'text-xs'} opacity-60 w-10 text-right tabular-nums"
			>{formatTime(playerStore.progress)}</span
		>
		<input
			type="range"
			class="range {size === 'stage' ? 'range-sm' : 'range-xs'} range-primary flex-1"
			class:opacity-50={!playerStore.isSeekable}
			class:cursor-not-allowed={!playerStore.isSeekable}
			min="0"
			max={playerStore.duration || 1}
			value={playerStore.progress}
			disabled={!playerStore.isSeekable}
			oninput={handleSeek}
			aria-label="Seek"
		/>
		<span class="{size === 'stage' ? 'text-sm' : 'text-xs'} opacity-60 w-10 tabular-nums"
			>{formatTime(playerStore.duration)}</span
		>
	</div>
	{#if showHint && !playerStore.isSeekable}
		<p class="text-[10px] text-base-content/60">This stream doesn't support seeking.</p>
	{/if}
</div>
