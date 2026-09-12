<script lang="ts">
	import { Disc3, LoaderCircle, Pause, Play, SkipForward, Volume2, X } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { deckSampler } from '$lib/stores/deckSampler.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { albumHrefOrNull, artistHrefOrNull } from '$lib/utils/entityRoutes';

	const visible = $derived(
		deckSampler.status === 'loading' ||
			deckSampler.status === 'playing' ||
			deckSampler.status === 'paused'
	);
	const entry = $derived(deckSampler.currentEntry);
	const albumHref = $derived(albumHrefOrNull(entry?.albumMbid));
	const artistHref = $derived(artistHrefOrNull(entry?.artistMbid));
	const providerLabel = $derived(
		deckSampler.provider === 'deezer'
			? 'Deezer'
			: deckSampler.provider === 'itunes'
				? 'iTunes'
				: null
	);

	// One-sound rule, enforced from an always-mounted surface: when the real player
	// STARTS (rising edge of isPlaying), the preview yields. Edge-triggered, not
	// level-triggered: a preview starting pauses the player asynchronously, so at the
	// microtask this effect first runs isPlaying is still true - a level check would
	// immediately kill the just-started preview. (audioFocus handles the reverse.)
	let wasPlaying = false;
	$effect(() => {
		const playing = playerStore.isPlaying;
		if (playing && !wasPlaying && deckSampler.status !== 'idle') {
			deckSampler.stop();
		}
		wasPlaying = playing;
	});
</script>

{#if visible && entry}
	<div
		class="preview-widget fixed right-3 z-40 w-[min(20rem,calc(100vw-1.5rem))] overflow-hidden rounded-2xl border border-secondary/20 bg-base-300/95 shadow-[0_8px_32px_rgba(0,0,0,0.4)] backdrop-blur-md"
		class:preview-widget--player={playerStore.isPlayerVisible}
		role="region"
		aria-label="Album preview"
	>
		<!-- progress: thin bar across the top -->
		<div class="h-0.5 w-full bg-base-content/10">
			<div
				class="h-full bg-secondary transition-[width] duration-100 ease-linear motion-reduce:transition-none"
				style="width: {Math.round(deckSampler.progress * 100)}%"
			></div>
		</div>

		<div class="flex items-center gap-3 p-3">
			<!-- cover -->
			<div class="relative h-14 w-14 shrink-0 overflow-hidden rounded-lg shadow-md">
				{#if entry.albumMbid}
					<AlbumImage
						mbid={entry.albumMbid}
						alt={entry.title}
						size="full"
						requestSize={250}
						rounded="none"
						className="block h-full w-full object-cover"
						customUrl={entry.coverUrl || null}
					/>
				{:else}
					<div class="flex h-full w-full items-center justify-center bg-base-100">
						<Disc3 class="h-7 w-7 text-base-content/30" />
					</div>
				{/if}
				{#if deckSampler.status === 'loading'}
					<div class="absolute inset-0 flex items-center justify-center bg-black/40">
						<LoaderCircle class="h-6 w-6 animate-spin text-white" />
					</div>
				{/if}
			</div>

			<!-- meta -->
			<div class="min-w-0 flex-1">
				<div class="flex items-center gap-1.5">
					<span class="text-[0.6rem] font-semibold uppercase tracking-wider text-secondary/80">
						{deckSampler.isStation ? 'Preview station' : 'Now sampling'}
					</span>
					{#if deckSampler.isStation}
						<span class="text-[0.6rem] text-base-content/40">
							{deckSampler.stationPosition.index + 1}/{deckSampler.stationPosition.total}
						</span>
					{/if}
				</div>
				<svelte:element
					this={albumHref ? 'a' : 'span'}
					href={albumHref ?? undefined}
					class="block truncate text-sm font-semibold {albumHref
						? 'transition-colors hover:text-secondary'
						: ''}"
					title={entry.title}
				>
					{entry.title}
				</svelte:element>
				<svelte:element
					this={artistHref ? 'a' : 'span'}
					href={artistHref ?? undefined}
					class="block truncate text-xs text-base-content/55 {artistHref
						? 'transition-colors hover:text-secondary'
						: ''}"
				>
					{entry.artist}{#if providerLabel}<span class="text-base-content/30">
							· via {providerLabel}</span
						>{/if}
				</svelte:element>
			</div>

			<!-- controls -->
			<div class="flex shrink-0 items-center gap-0.5">
				<button
					class="btn btn-circle btn-secondary btn-sm"
					onclick={() => deckSampler.togglePlay()}
					aria-label={deckSampler.status === 'paused' ? 'Resume preview' : 'Pause preview'}
					disabled={deckSampler.status === 'loading'}
				>
					{#if deckSampler.status === 'paused'}
						<Play class="h-4 w-4" fill="currentColor" />
					{:else}
						<Pause class="h-4 w-4" fill="currentColor" />
					{/if}
				</button>
				{#if deckSampler.hasNext}
					<button
						class="btn btn-circle btn-ghost btn-sm"
						onclick={() => deckSampler.next()}
						aria-label="Skip to next album"
						title="Skip to next album"
						disabled={deckSampler.status === 'loading'}
					>
						<SkipForward class="h-4 w-4" />
					</button>
				{/if}
				<button
					class="btn btn-circle btn-ghost btn-sm"
					onclick={() => deckSampler.stop()}
					aria-label="Close preview"
					title="Stop preview"
				>
					<X class="h-4 w-4" />
				</button>
			</div>
		</div>

		<!-- volume -->
		<div class="flex items-center gap-2 px-3 pb-2.5">
			<Volume2 class="h-3.5 w-3.5 shrink-0 text-base-content/40" />
			<input
				type="range"
				min="0"
				max="1"
				step="0.05"
				value={deckSampler.volume}
				oninput={(e) => deckSampler.setVolume(Number(e.currentTarget.value))}
				class="range range-secondary range-xs flex-1"
				aria-label="Preview volume"
			/>
		</div>
	</div>
{/if}

<style>
	/* sit a line above the centered playback toast (same bottom band, z-50) so a
	   transient toast never overlaps the persistent widget */
	.preview-widget {
		bottom: calc(var(--ms-bottom-nav-offset) + 4rem);
	}
	.preview-widget.preview-widget--player {
		bottom: calc(var(--ms-bottom-nav-offset) + var(--ms-player-height) + 4rem);
	}
	@media (prefers-reduced-motion: no-preference) {
		.preview-widget {
			animation: preview-widget-in 0.28s cubic-bezier(0.16, 1, 0.3, 1);
		}
	}
	@keyframes preview-widget-in {
		from {
			opacity: 0;
			transform: translateY(0.75rem);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}
</style>
