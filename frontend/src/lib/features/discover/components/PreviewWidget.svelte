<!--
	The sample playing anywhere in the app but the Discover page, where the
	listening post shows it: the album or pick, the clip and what it is to its
	album, where it is from, and its controls. A Discover pick also offers
	Keep and Later, as plain requests, until it is answered here.
-->
<script lang="ts">
	import { LoaderCircle, Pause, Play, SkipForward, Volume2, X } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import { deckSampler, type SampleEntry } from '$lib/features/discover/state/deckSampler.svelte';
	import { pickAnswers } from '$lib/features/discover/state/pickAnswers.svelte';
	import { createKeepPickMutation, quotaRefusal } from '$lib/features/discover/queries/week.svelte';
	import { createMarkLaterMutation } from '$lib/features/discover/queries/later.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { albumHrefOrNull, artistHrefOrNull } from '$lib/utils/entityRoutes';

	const visible = $derived(
		!pickAnswers.current &&
			(deckSampler.status === 'loading' ||
				deckSampler.status === 'playing' ||
				deckSampler.status === 'paused')
	);
	const entry = $derived(deckSampler.currentEntry);
	const albumHref = $derived(albumHrefOrNull(entry?.albumMbid));
	const artistHref = $derived(artistHrefOrNull(entry?.artistMbid));
	const SOURCES = { deezer: 'Deezer', itunes: 'iTunes' } as const;
	const ROLES = { opener: 'Opener', best_known: 'Best known', deep_cut: 'Deep cut' } as const;
	const providerLabel = $derived.by(() => {
		const from = deckSampler.currentTrack?.source ?? deckSampler.provider;
		return from === 'deezer' || from === 'itunes' ? SOURCES[from] : null;
	});
	const role = $derived.by(() => {
		const of = deckSampler.currentTrack?.role;
		return of ? ROLES[of] : null;
	});

	// Keep and Later off the Discover page: plain requests, with their own toasts
	const keep = createKeepPickMutation();
	const markLater = createMarkLaterMutation();
	const answeredHere = new SvelteSet<string>();
	const answerable = $derived.by(() => {
		if (!entry?.pick || !entry.albumMbid) return false;
		return !answeredHere.has(entry.key);
	});

	/** Moves past a pick answered here: the station's next album, or the end. */
	function answered(key: string) {
		answeredHere.add(key);
		if (deckSampler.activeKey !== key) return;
		if (deckSampler.hasNext) deckSampler.next();
		else deckSampler.stop();
	}

	async function onkeep(sample: SampleEntry) {
		if (!sample.pick || !sample.albumMbid) return;
		try {
			await keep.mutateAsync({
				release_group_mbid: sample.albumMbid,
				artist_name: sample.artist || null,
				album_title: sample.title,
				artist_mbid: sample.artistMbid ?? null,
				made_from: sample.pick
			});
		} catch (error) {
			const refusal = quotaRefusal(error);
			if (refusal) {
				toastStore.show({
					message:
						refusal.limit !== null && refusal.window_days !== null
							? `Request limit reached: ${refusal.limit} albums every ${refusal.window_days} days, all used.`
							: 'Request limit reached.',
					type: 'warning'
				});
			}
			return;
		}
		answered(sample.key);
	}

	async function onlater(sample: SampleEntry) {
		if (!sample.albumMbid) return;
		try {
			await markLater.mutateAsync({
				mbid: sample.albumMbid,
				title: sample.title,
				artist_name: sample.artist,
				artist_mbid: sample.artistMbid ?? null
			});
		} catch {
			return;
		}
		answered(sample.key);
	}

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
		class="preview-widget fixed right-3 z-40 w-[min(20rem,calc(100vw-1.5rem))] overflow-hidden rounded-box border border-line bg-base-200 shadow-xl"
		class:preview-widget--player={playerStore.isPlayerVisible}
		role="region"
		aria-label="Album preview"
	>
		<!-- progress: thin bar across the top -->
		<div class="h-0.5 w-full bg-base-300">
			<div
				class="h-full w-full origin-left bg-action transition-[scale] duration-100 ease-linear motion-reduce:transition-none"
				style:scale="{deckSampler.progress} 1"
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
					<div class="relative h-full w-full overflow-hidden">
						<CoverPlaceholder id={entry.albumMbid} name={entry.title} />
					</div>
				{/if}
				{#if deckSampler.status === 'loading'}
					<div class="absolute inset-0 flex items-center justify-center bg-black/40">
						<LoaderCircle class="h-6 w-6 animate-spin motion-keep text-white" />
					</div>
				{/if}
			</div>

			<!-- meta -->
			<div class="min-w-0 flex-1">
				<div class="flex items-center gap-1.5">
					<span
						class="truncate font-mono text-xs font-semibold uppercase tracking-wider text-muted"
					>
						{role ?? (deckSampler.isStation ? 'Preview station' : 'Now sampling')}
					</span>
					{#if deckSampler.isStation}
						<span class="shrink-0 font-mono text-xs text-muted">
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
					class="block truncate text-xs text-muted {artistHref
						? 'transition-colors hover:text-secondary'
						: ''}"
				>
					{entry.artist}{#if providerLabel}<span class="text-muted">
							· via {providerLabel}
						</span>{/if}
				</svelte:element>
			</div>

			<!-- controls -->
			<div class="flex shrink-0 items-center gap-0.5">
				<IconButton
					variant="accent"
					size="sm"
					icon={deckSampler.status === 'paused' ? Play : Pause}
					label={deckSampler.status === 'paused' ? 'Resume preview' : 'Pause preview'}
					class="[&_svg]:fill-current"
					disabled={deckSampler.status === 'loading'}
					onclick={() => deckSampler.togglePlay()}
				/>
				{#if deckSampler.hasNext}
					<IconButton
						size="sm"
						icon={SkipForward}
						label="Skip to next album"
						disabled={deckSampler.status === 'loading'}
						onclick={() => deckSampler.next()}
					/>
				{/if}
				<IconButton size="sm" icon={X} label="Close preview" onclick={() => deckSampler.stop()} />
			</div>
		</div>

		{#if answerable && entry}
			<div class="flex gap-2 px-3 pb-2">
				<Button variant="primary" size="xs" class="flex-1" onclick={() => void onkeep(entry)}>
					Keep
				</Button>
				<Button variant="outline" size="xs" class="flex-1" onclick={() => void onlater(entry)}>
					Later
				</Button>
			</div>
		{/if}

		<!-- volume -->
		<div class="flex items-center gap-2 px-3 pb-2.5">
			<Volume2 class="h-3.5 w-3.5 shrink-0 text-muted" />
			<input
				type="range"
				min="0"
				max="1"
				step="0.05"
				value={deckSampler.volume}
				oninput={(e) => deckSampler.setVolume(Number(e.currentTarget.value))}
				class="range range-primary range-xs flex-1"
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
