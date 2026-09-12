<script lang="ts">
	import { X, Music2, LoaderCircle, CircleAlert } from 'lucide-svelte';
	import { slide } from 'svelte/transition';
	import type { LyricLine } from '$lib/types';

	interface Props {
		open: boolean;
		lyricsText: string;
		lines?: LyricLine[];
		isSynced?: boolean;
		isLoading?: boolean;
		hasError?: boolean;
		currentTime?: number;
		trackName?: string;
		artistName?: string;
		onclose: () => void;
	}

	let {
		open = $bindable(),
		lyricsText,
		lines = [],
		isSynced = false,
		isLoading = false,
		hasError = false,
		currentTime = 0,
		trackName = '',
		artistName = '',
		onclose
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

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			open = false;
			onclose();
		}
	}
</script>

{#if open}
	<div
		class="fixed inset-x-0 bottom-[90px] z-40 flex justify-center pointer-events-none"
		role="dialog"
		aria-label="Lyrics"
		aria-modal="true"
		tabindex="-1"
		onkeydown={handleKeydown}
	>
		<div
			class="pointer-events-auto w-full max-w-lg mx-4 bg-base-200 rounded-t-2xl shadow-xl border border-base-300 flex flex-col max-h-[60vh]"
			transition:slide={{ duration: 250 }}
		>
			<div class="flex items-center justify-between px-4 py-3 border-b border-base-300">
				<div class="flex items-center gap-2 min-w-0">
					<Music2 class="h-4 w-4 text-primary shrink-0" />
					<div class="min-w-0">
						{#if trackName}
							<p class="text-sm font-semibold truncate">{trackName}</p>
						{/if}
						{#if artistName}
							<p class="text-xs text-base-content/60 truncate">{artistName}</p>
						{/if}
					</div>
				</div>
				<button
					class="btn btn-ghost btn-sm btn-circle"
					onclick={() => {
						open = false;
						onclose();
					}}
					aria-label="Close lyrics"
				>
					<X class="h-4 w-4" />
				</button>
			</div>

			<div
				bind:this={scrollContainer}
				class="overflow-y-auto px-6 py-4 flex-1"
				onscroll={onUserScroll}
			>
				{#if isLoading}
					<div class="flex flex-col items-center justify-center py-12 gap-3">
						<LoaderCircle class="h-6 w-6 animate-spin text-primary" />
						<p class="text-sm text-base-content/50">Loading lyrics...</p>
					</div>
				{:else if hasError}
					<div class="flex flex-col items-center justify-center py-8 gap-2">
						<CircleAlert class="h-5 w-5 text-warning" />
						<p class="text-center text-base-content/50 text-sm">
							Couldn't load the lyrics. Try again in a bit.
						</p>
					</div>
				{:else if timedLines.length > 0}
					<div class="space-y-2">
						{#each timedLines as line, i (i)}
							<p
								data-line={i}
								class="text-sm leading-relaxed transition-all duration-300
									{i === activeLineIndex ? 'text-primary font-semibold' : ''}
									{i !== activeLineIndex && i < activeLineIndex ? 'opacity-80' : ''}
									{i > activeLineIndex ? 'opacity-40' : ''}"
							>
								{line.text}
							</p>
						{/each}
					</div>
				{:else if lyricsText.trim()}
					<pre
						class="whitespace-pre-wrap font-sans text-sm leading-relaxed text-base-content/80">{lyricsText}</pre>
				{:else}
					<p class="text-center text-base-content/40 py-8">
						Lyrics aren't available for this track.
					</p>
				{/if}
			</div>

			{#if isSynced}
				<div class="px-4 py-2 border-t border-base-300">
					<span class="badge badge-xs badge-primary">Synced</span>
				</div>
			{/if}
		</div>
	</div>
{/if}
