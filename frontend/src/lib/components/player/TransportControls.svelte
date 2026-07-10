<script lang="ts">
	/*
	 * TransportControls — shuffle / previous / play-pause / next.
	 * One component, two densities: 'dock' (compact bar) and 'stage' (large).
	 * Error state turns the primary button into a dismiss action, exactly like
	 * the previous player bar.
	 */
	import { playerStore } from '$lib/stores/player.svelte';
	import { Shuffle, SkipBack, SkipForward, Play, Pause, AlertCircle } from 'lucide-svelte';

	interface Props {
		size?: 'dock' | 'stage';
	}

	let { size = 'dock' }: Props = $props();

	const isStage = $derived(size === 'stage');
	const mainBtn = $derived(isStage ? 'w-16 h-16' : 'w-10 h-10');
	const mainIcon = $derived(isStage ? 'h-8 w-8' : 'h-5 w-5');
	const sideIcon = $derived(isStage ? 'h-6 w-6' : 'h-4 w-4');
	const sideBtn = $derived(isStage ? 'btn-md' : 'btn-sm');
</script>

<div class="flex items-center {isStage ? 'gap-4 sm:gap-6' : 'gap-1 sm:gap-3'}">
	<!-- the Stage centres prev/play/next symmetrically; its shuffle lives in the
	     queue panel header instead of here -->
	{#if playerStore.hasQueue && !isStage}
		<button
			class="btn btn-ghost {sideBtn} btn-circle {isStage ? '' : 'hidden sm:inline-flex'}"
			class:text-accent={playerStore.shuffleEnabled}
			class:opacity-50={!playerStore.shuffleEnabled}
			onclick={() => playerStore.toggleShuffle()}
			aria-label="Toggle shuffle"
			aria-pressed={playerStore.shuffleEnabled}
		>
			<Shuffle class={isStage ? 'h-5 w-5' : 'h-4 w-4'} />
		</button>
	{/if}

	<button
		class="btn btn-ghost {sideBtn} btn-circle"
		class:opacity-30={!playerStore.hasPrevious}
		class:cursor-not-allowed={!playerStore.hasPrevious}
		disabled={!playerStore.hasPrevious}
		onclick={() => playerStore.previousTrack()}
		aria-label="Previous"
	>
		<SkipBack class="{sideIcon} fill-current" />
	</button>

	<button
		class="btn btn-circle btn-primary shadow-md {mainBtn}"
		onclick={() =>
			playerStore.playbackState === 'error' ? playerStore.stop() : playerStore.togglePlay()}
		aria-label={playerStore.playbackState === 'error'
			? 'Close'
			: playerStore.isPlaying
				? 'Pause'
				: 'Play'}
	>
		{#if playerStore.playbackState === 'error'}
			<AlertCircle class={mainIcon} />
		{:else if playerStore.isBuffering}
			<span class="loading loading-spinner {isStage ? 'loading-md' : 'loading-sm'}"></span>
		{:else if playerStore.isPlaying}
			<Pause class="{mainIcon} fill-current" />
		{:else}
			<Play class="{mainIcon} ml-0.5 fill-current" />
		{/if}
	</button>

	<button
		class="btn btn-ghost {sideBtn} btn-circle"
		class:opacity-30={!playerStore.hasNext}
		class:cursor-not-allowed={!playerStore.hasNext}
		disabled={!playerStore.hasNext}
		onclick={() => playerStore.nextTrack()}
		aria-label="Next"
	>
		<SkipForward class="{sideIcon} fill-current" />
	</button>
</div>
