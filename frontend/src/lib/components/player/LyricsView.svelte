<script lang="ts">
	/*
	 * LyricsView — the lyrics body shared by the LyricsPanel and the Stage.
	 * Synced auto-scroll (paused for 3s after the user scrolls) or plain text.
	 */
	import { Loader2, AlertCircle } from 'lucide-svelte';
	import type { LyricLine } from '$lib/types';

	interface Props {
		lyricsText: string;
		lines?: LyricLine[];
		isSynced?: boolean;
		isLoading?: boolean;
		hasError?: boolean;
		currentTime?: number;
		/** 'panel' = compact dock popover, 'stage' = large immersive type */
		size?: 'panel' | 'stage';
	}

	let {
		lyricsText,
		lines = [],
		isSynced = false,
		isLoading = false,
		hasError = false,
		currentTime = 0,
		size = 'panel'
	}: Props = $props();

	let scrollContainer: HTMLDivElement | undefined = $state();
	let userScrolling = $state(false);
	let scrollTimeout: ReturnType<typeof setTimeout> | undefined;

	const timedLines = $derived(
		isSynced && lines.length > 0 ? lines.filter((l) => l.start_seconds !== null) : []
	);

	const activeLineIndex = $derived.by(() => {
		if (timedLines.length === 0) return -1;
		let idx = -1;
		for (let i = 0; i < timedLines.length; i++) {
			if ((timedLines[i].start_seconds ?? 0) <= currentTime) {
				idx = i;
			} else {
				break;
			}
		}
		return idx;
	});

	$effect(() => {
		if (activeLineIndex < 0 || userScrolling || !scrollContainer) return;
		const el = scrollContainer.querySelector(`[data-line="${activeLineIndex}"]`);
		if (el) {
			el.scrollIntoView({ behavior: 'smooth', block: 'center' });
		}
	});

	function onUserScroll() {
		userScrolling = true;
		clearTimeout(scrollTimeout);
		scrollTimeout = setTimeout(() => {
			userScrolling = false;
		}, 3000);
	}

	const lineClass = $derived(
		size === 'stage' ? 'text-lg sm:text-xl font-medium leading-relaxed' : 'text-sm leading-relaxed'
	);
</script>

<div
	bind:this={scrollContainer}
	class="overflow-y-auto flex-1 {size === 'stage' ? 'px-2 py-6' : 'px-6 py-4'}"
	onscroll={onUserScroll}
>
	{#if isLoading}
		<div class="flex flex-col items-center justify-center py-12 gap-3">
			<Loader2 class="h-6 w-6 animate-spin text-primary" />
			<p class="text-sm text-base-content/50">Loading lyrics...</p>
		</div>
	{:else if hasError}
		<div class="flex flex-col items-center justify-center py-8 gap-2">
			<AlertCircle class="h-5 w-5 text-warning" />
			<p class="text-center text-base-content/50 text-sm">
				Couldn't load the lyrics. Try again in a bit.
			</p>
		</div>
	{:else if timedLines.length > 0}
		<div class={size === 'stage' ? 'space-y-4' : 'space-y-2'}>
			{#each timedLines as line, i (i)}
				<p
					data-line={i}
					class="{lineClass} transition-all duration-300
						{i === activeLineIndex ? 'text-primary font-semibold' : ''}
						{i !== activeLineIndex && i < activeLineIndex ? 'opacity-80' : ''}
						{i > activeLineIndex ? 'opacity-40' : ''}"
				>
					{line.text}
				</p>
			{/each}
		</div>
	{:else if lyricsText.trim()}
		<pre class="whitespace-pre-wrap font-sans {lineClass} text-base-content/80">{lyricsText}</pre>
	{:else}
		<p class="text-center text-base-content/40 py-8">Lyrics aren't available for this track.</p>
	{/if}
</div>
