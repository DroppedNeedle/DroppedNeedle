<!--
	The lyrics of the playing track, for the drawer's Lyrics tab and the
	Listening Room's sheet. Timed lines follow the song: the current one is
	bold on a marker stroke, lines sung are muted, lines to come quieter
	still, and the list keeps the current line in its middle until the
	listener scrolls it by hand (for three seconds). Plain lyrics read as
	written.
-->
<script lang="ts">
	import { CircleAlert, LoaderCircle } from 'lucide-svelte';
	import { prefersReducedMotion } from 'svelte/motion';
	import { isValidMbid } from '$lib/utils/formatting';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import type { LyricLine } from '$lib/features/player/queries/types';

	interface Props {
		lyricsText: string;
		lines?: LyricLine[];
		isSynced?: boolean;
		isLoading?: boolean;
		hasError?: boolean;
		currentTime?: number;
		trackName?: string;
		artistName?: string;
	}

	let {
		lyricsText,
		lines = [],
		isSynced = false,
		isLoading = false,
		hasError = false,
		currentTime = 0,
		trackName = '',
		artistName = ''
	}: Props = $props();

	let scroller = $state<HTMLDivElement | undefined>();
	let userScrolling = false;
	let scrollTimeout: ReturnType<typeof setTimeout> | undefined;

	const nowPlaying = $derived(playerStore.nowPlaying);
	const timedLines = $derived(
		isSynced && lines.length > 0 ? lines.filter((l) => l.start_seconds !== null) : []
	);

	const activeLineIndex = $derived.by(() => {
		let idx = -1;
		for (let i = 0; i < timedLines.length; i++) {
			if ((timedLines[i].start_seconds ?? 0) <= currentTime) idx = i;
			else break;
		}
		return idx;
	});

	$effect(() => {
		if (activeLineIndex < 0 || !scroller) return;
		const box = scroller;
		const line = box.querySelector<HTMLElement>(`[data-line="${activeLineIndex}"]`);
		if (!line || userScrolling) return;
		const offset = line.getBoundingClientRect().top - box.getBoundingClientRect().top;
		box.scrollTo({
			top: box.scrollTop + offset - box.clientHeight / 2 + line.offsetHeight / 2,
			behavior: prefersReducedMotion.current ? 'instant' : 'smooth'
		});
	});

	// A wheel, a touch or a drag on the scrollbar is the listener reading ahead.
	function onUserScroll(): void {
		userScrolling = true;
		clearTimeout(scrollTimeout);
		scrollTimeout = setTimeout(() => (userScrolling = false), 3000);
	}

	function watchReading(node: HTMLElement) {
		const events = ['wheel', 'touchmove', 'pointerdown'] as const;
		for (const name of events) node.addEventListener(name, onUserScroll, { passive: true });
		return {
			destroy() {
				for (const name of events) node.removeEventListener(name, onUserScroll);
				clearTimeout(scrollTimeout);
			}
		};
	}
</script>

{#if trackName || artistName}
	<div class="flex items-center gap-3 border-b border-line px-4 py-3">
		{#if nowPlaying}
			<AlbumImage
				mbid={isValidMbid(nowPlaying.albumId) ? nowPlaying.albumId : ''}
				customUrl={nowPlaying.coverUrl ?? null}
				alt={nowPlaying.albumName}
				size="full"
				requestSize={250}
				rounded="sm"
				lazy={false}
				className="size-11 shrink-0"
			/>
		{/if}
		<div class="min-w-0">
			{#if trackName}<p class="truncate text-sm font-bold">{trackName}</p>{/if}
			{#if artistName}<p class="truncate text-xs text-muted">{artistName}</p>{/if}
		</div>
	</div>
{/if}

<div bind:this={scroller} class="min-h-0 flex-1 overflow-y-auto px-5 py-5" use:watchReading>
	{#if isLoading}
		<div class="flex flex-col items-center justify-center gap-3 py-12">
			<LoaderCircle class="motion-keep size-6 animate-spin text-primary" aria-hidden="true" />
			<p class="text-sm text-muted">Loading lyrics...</p>
		</div>
	{:else if hasError}
		<div class="flex flex-col items-center justify-center gap-2 py-8 text-center">
			<CircleAlert class="size-5 text-warning" aria-hidden="true" />
			<p class="text-sm text-muted">Couldn't load the lyrics. Try again in a bit.</p>
		</div>
	{:else if timedLines.length > 0}
		<div class="dn-lyrics" data-lyrics>
			{#each timedLines as line, i (i)}
				<p
					data-line={i}
					class="dn-lyric"
					class:dn-lyric--now={i === activeLineIndex}
					class:dn-lyric--sung={i < activeLineIndex}
				>
					<span>{line.text}</span>
				</p>
			{/each}
		</div>
	{:else if lyricsText.trim()}
		<pre class="dn-lyrics-plain" data-lyrics>{lyricsText}</pre>
	{:else}
		<p class="py-8 text-center text-muted">Lyrics aren't available for this track.</p>
	{/if}
</div>

{#if isSynced && !isLoading && !hasError}
	<div class="border-t border-line px-4 py-2.5">
		<StatusChip size="xs">Synced</StatusChip>
	</div>
{/if}

<style>
	.dn-lyrics {
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}
	/* lines to come are the quietest; muted text one step back toward the surface */
	.dn-lyric {
		font-size: 1rem;
		line-height: 1.45;
		color: color-mix(in oklab, var(--dn-muted) 64%, var(--color-base-200));
		transition: color 250ms;
	}
	.dn-lyric > span {
		display: inline-block;
		padding: 0.125rem 0;
	}
	.dn-lyric--sung {
		color: var(--dn-muted);
	}
	/* the current line: bold on a marker stroke whose right end is ragged */
	.dn-lyric--now {
		color: var(--dn-action-content);
	}
	.dn-lyric--now > span {
		margin-left: -0.5rem;
		padding: 0.125rem 1.75rem 0.125rem 0.5rem;
		background: var(--dn-action);
		font-weight: 700;
		clip-path: polygon(0 6%, 100% 0, 97% 22%, 99.5% 46%, 96.5% 68%, 98.5% 88%, 95% 100%, 0 96%);
	}
	.dn-lyrics-plain {
		font-family: var(--font-sans);
		font-size: 1rem;
		line-height: 1.6;
		white-space: pre-wrap;
	}
	@media (forced-colors: active) {
		.dn-lyric--now > span {
			clip-path: none;
			text-decoration: underline;
		}
	}
</style>
