<script lang="ts">
	import '$lib/features/player/components/now-playing.css';
	import '$lib/features/library/components/local/deck.css';
	import type { CrateTrack, CrateReason } from '$lib/features/library/queries/types';
	import { flip, fly } from '$lib/ui/motion';
	import { crateCoverUrl, type SpinAlbum } from '$lib/features/library/queries/LocalV3Adapters';
	import {
		Sparkles,
		RotateCcw,
		Dices,
		Clock,
		Play,
		ListPlus,
		ListStart,
		GripVertical,
		RefreshCw,
		ArrowDownToLine
	} from 'lucide-svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import ContextMenu, { menuTrigger, type MenuPoint } from '$lib/ui/ContextMenu.svelte';

	interface Props {
		tracks: CrateTrack[];
		isLoading?: boolean;
		reducedMotion?: boolean;
		// 1 = just refreshed, 0 = about to refresh
		countdownFraction?: number;
		// bumped on every refresh to replay the deal-in + light-sweep
		refreshNonce?: number;
		// auto-scroll is timed to finish just before this cadence
		refreshIntervalMs?: number;
		upcomingCount?: number;
		onRefresh: () => void;
		onPlay: (t: CrateTrack) => void;
		onQueue: (t: CrateTrack) => void;
		onPlayNext: (t: CrateTrack) => void;
		onQueueAlbum?: (a: SpinAlbum) => void;
	}

	let {
		tracks,
		isLoading = false,
		reducedMotion = false,
		countdownFraction = 1,
		refreshNonce = 0,
		refreshIntervalMs = 35_000,
		upcomingCount = 0,
		onRefresh,
		onPlay,
		onQueue,
		onPlayNext,
		onQueueAlbum
	}: Props = $props();

	// drift to the bottom so every record is seen, finishing ~2s before the next
	// refresh; a manual scroll cancels it for the cycle. disabled under reduced-motion
	const SCROLL_START_BUFFER_MS = 2000;
	const SCROLL_END_BUFFER_MS = 2000;
	let scrollEl: HTMLDivElement | undefined = $state();
	let rafId = 0;
	let startTimer: ReturnType<typeof setTimeout> | null = null;
	let userInterrupted = false;

	function stopAutoScroll() {
		if (rafId) {
			cancelAnimationFrame(rafId);
			rafId = 0;
		}
		if (startTimer) {
			clearTimeout(startTimer);
			startTimer = null;
		}
	}

	function startAutoScroll() {
		stopAutoScroll();
		userInterrupted = false;
		const el = scrollEl;
		if (!el) return;
		el.scrollTop = 0;
		const duration = refreshIntervalMs - SCROLL_START_BUFFER_MS - SCROLL_END_BUFFER_MS;
		if (duration <= 0) return;
		startTimer = setTimeout(() => {
			startTimer = null;
			if (userInterrupted || !scrollEl) return;
			const maxScroll = scrollEl.scrollHeight - scrollEl.clientHeight;
			if (maxScroll <= 0) return;
			let elapsed = 0;
			let last = performance.now();
			const step = (now: number) => {
				if (userInterrupted || !scrollEl) return;
				// cap per-frame delta so a backgrounded tab doesn't jump on resume
				elapsed += Math.min(now - last, 100);
				last = now;
				const t = Math.min(1, elapsed / duration);
				scrollEl.scrollTop = maxScroll * t;
				if (t < 1) rafId = requestAnimationFrame(step);
			};
			rafId = requestAnimationFrame(step);
		}, SCROLL_START_BUFFER_MS);
	}

	function interruptAutoScroll() {
		userInterrupted = true;
		stopAutoScroll();
	}

	$effect(() => {
		// re-arm on every refresh; also tracks scrollEl/reducedMotion/interval
		void refreshNonce;
		void refreshIntervalMs;
		const el = scrollEl;
		if (!el || reducedMotion) return;
		// passive listeners keep the scroll container non-interactive; a manual scroll cancels the drift
		const onInterrupt = () => interruptAutoScroll();
		el.addEventListener('wheel', onInterrupt, { passive: true });
		el.addEventListener('touchmove', onInterrupt, { passive: true });
		el.addEventListener('pointerdown', onInterrupt, { passive: true });
		startAutoScroll();
		return () => {
			stopAutoScroll();
			el.removeEventListener('wheel', onInterrupt);
			el.removeEventListener('touchmove', onInterrupt);
			el.removeEventListener('pointerdown', onInterrupt);
		};
	});

	// must match the SVG circle r=16 used for the countdown stroke
	const RING_CIRCUMFERENCE = 2 * Math.PI * 16;
	const ringOffset = $derived(
		RING_CIRCUMFERENCE * (1 - Math.max(0, Math.min(1, countdownFraction)))
	);

	const REASONS: Record<
		CrateReason,
		{ label: string; icon: typeof Sparkles; tone: 'action' | 'neutral' }
	> = {
		recent: { label: 'Recently Added', icon: Sparkles, tone: 'action' },
		rediscover: { label: 'Rediscover', icon: RotateCcw, tone: 'neutral' },
		surprise: { label: 'Surprise', icon: Dices, tone: 'neutral' },
		same_era: { label: 'Same era', icon: Clock, tone: 'neutral' }
	};

	const secondsLeft = $derived(
		Math.max(0, Math.ceil((Math.max(0, Math.min(1, countdownFraction)) * refreshIntervalMs) / 1000))
	);

	const flipDur = $derived(reducedMotion ? 0 : 340);
	let draggingId = $state<string | null>(null);
	let dragOverQueue = $state(false);

	// one menu for the whole crate, opened from a row
	let menu = $state<(MenuPoint & { track: CrateTrack }) | null>(null);

	function openMenu(at: MenuPoint, track: CrateTrack) {
		// the drift would scroll under the menu and close it
		interruptAutoScroll();
		menu = { ...at, track };
	}

	function menuItems(t: CrateTrack) {
		return [
			{ label: 'Play', icon: Play, onclick: () => onPlay(t) },
			{ label: 'Play Next', icon: ListStart, onclick: () => onPlayNext(t) },
			{ label: 'Add to Queue', icon: ListPlus, onclick: () => onQueue(t) }
		];
	}

	function onDragStart(e: DragEvent, t: CrateTrack) {
		if (!e.dataTransfer) return;
		e.dataTransfer.setData('application/x-crate-track', JSON.stringify(t));
		e.dataTransfer.effectAllowed = 'copy';
		draggingId = t.track_file_id;
	}

	function readTrack(e: DragEvent): CrateTrack | null {
		const raw = e.dataTransfer?.getData('application/x-crate-track');
		if (!raw) return null;
		try {
			return JSON.parse(raw) as CrateTrack;
		} catch {
			return null;
		}
	}

	function readAlbum(e: DragEvent): SpinAlbum | null {
		const raw = e.dataTransfer?.getData('application/x-crate-album');
		if (!raw) return null;
		try {
			return JSON.parse(raw) as SpinAlbum;
		} catch {
			return null;
		}
	}

	function queueAccepts(e: DragEvent): boolean {
		const types = e.dataTransfer?.types;
		return (
			!!types &&
			(types.includes('application/x-crate-track') || types.includes('application/x-crate-album'))
		);
	}

	function onQueueDrop(e: DragEvent) {
		e.preventDefault();
		dragOverQueue = false;
		const t = readTrack(e);
		if (t) {
			onQueue(t);
			return;
		}
		const a = readAlbum(e);
		if (a) onQueueAlbum?.(a);
	}
</script>

<section class="lr-panel">
	<header class="lr-panel__head">
		<h2 class="lr-panel__title">In the crate</h2>
		<div class="flex items-center gap-2">
			<span class="crate-countdown" aria-hidden="true">Refresh in {secondsLeft}s</span>
			<div class="relative grid size-8 place-items-center" title="Next mix loads automatically">
				<svg
					class="pointer-events-none absolute inset-0 h-full w-full -rotate-90"
					viewBox="0 0 36 36"
					aria-hidden="true"
				>
					<circle cx="18" cy="18" r="16" fill="none" stroke-width="2" class="crate-ring-track" />
					<circle
						cx="18"
						cy="18"
						r="16"
						fill="none"
						stroke-width="2.5"
						stroke-linecap="round"
						class="countdown-ring"
						stroke-dasharray={RING_CIRCUMFERENCE}
						stroke-dashoffset={ringOffset}
					/>
				</svg>
				<IconButton
					size="xs"
					icon={RefreshCw}
					label="Shuffle the crate now"
					loading={isLoading}
					onclick={onRefresh}
				/>
			</div>
		</div>
	</header>

	<div class="relative min-h-0 flex-1 overflow-hidden">
		<div bind:this={scrollEl} class="h-full space-y-0.5 overflow-y-auto">
			{#if tracks.length === 0 && isLoading}
				{#each Array(5) as _, i (i)}
					<div class="flex items-center gap-2.5 px-1.5 py-[0.4375rem]">
						<div class="skeleton size-12 shrink-0 rounded"></div>
						<div class="flex-1 space-y-1.5">
							<div class="skeleton h-3.5 w-3/5"></div>
							<div class="skeleton h-3 w-2/5"></div>
						</div>
					</div>
				{/each}
			{:else}
				{#each tracks as t, i (t.track_file_id)}
					{@const meta = REASONS[t.reason]}
					{@const cover = crateCoverUrl(t)}
					<div
						class="lr-row group"
						class:is-dragging={draggingId === t.track_file_id}
						draggable="true"
						role="button"
						tabindex="0"
						aria-label="{t.title} by {t.artist_name}"
						aria-haspopup="menu"
						use:menuTrigger={{ open: (at) => openMenu(at, t), close: () => (menu = null) }}
						ondragstart={(e) => onDragStart(e, t)}
						ondragend={() => (draggingId = null)}
						ondblclick={() => onPlay(t)}
						onkeydown={(e) => {
							if (e.target !== e.currentTarget) return;
							if (e.key === 'Enter' || e.key === ' ') {
								e.preventDefault();
								onPlay(t);
							}
						}}
						animate:flip={{ duration: flipDur }}
						in:fly={{ y: 16, duration: reducedMotion ? 0 : 280, delay: reducedMotion ? 0 : i * 45 }}
						out:fly={{ y: -16, duration: reducedMotion ? 0 : 260 }}
					>
						<GripVertical class="size-4 shrink-0 text-muted" aria-hidden="true" />
						<div class="lr-row__cover">
							{#if cover}
								<AlbumImage
									customUrl={cover}
									alt={t.album_name}
									size="full"
									rounded="none"
									className="h-full w-full"
								/>
							{:else}
								<CoverPlaceholder name={t.album_name} />
							{/if}
						</div>
						<div class="min-w-0 flex-1">
							<p class="lr-row__title">{t.title}</p>
							<p class="lr-row__sub">{t.artist_name}</p>
							<StatusChip tone={meta.tone} size="xs" icon={meta.icon} class="mt-1">
								{meta.label}
							</StatusChip>
						</div>
						<div class="lr-row__actions">
							<IconButton size="sm" icon={Play} label="Play now" onclick={() => onPlay(t)} />
							<IconButton
								size="sm"
								icon={ListPlus}
								label="Add to queue"
								onclick={() => onQueue(t)}
							/>
						</div>
					</div>
				{/each}
			{/if}
		</div>

		{#if !reducedMotion}
			{#key refreshNonce}
				<div class="crate-sweep pointer-events-none absolute inset-0 z-10" aria-hidden="true"></div>
			{/key}
		{/if}
	</div>

	<div
		class="upnext-zone deck-droptarget"
		class:is-over={dragOverQueue}
		role="region"
		aria-label="Up Next - drop a song or album here to add it to the queue"
		ondragover={(e) => {
			if (queueAccepts(e)) {
				e.preventDefault();
				dragOverQueue = true;
			}
		}}
		ondragleave={() => (dragOverQueue = false)}
		ondrop={onQueueDrop}
	>
		<div class="upnext-zone__icon">
			<ArrowDownToLine class="size-4 {dragOverQueue || reducedMotion ? '' : 'animate-float'}" />
		</div>
		<div class="min-w-0 text-left">
			<p class="upnext-zone__title">Up Next</p>
			<p class="truncate text-xs text-muted">Drop a song or album to queue</p>
		</div>
		{#if upcomingCount > 0}
			<StatusChip tone="action" size="sm" class="ml-auto">
				{upcomingCount}<span class="sr-only"> in the queue</span>
			</StatusChip>
		{/if}
	</div>
</section>

{#if menu}
	<ContextMenu
		trigger="point"
		at={menu}
		label="{menu.track.title} by {menu.track.artist_name}"
		items={menuItems(menu.track)}
		onclose={() => (menu = null)}
	/>
{/if}

<style>
	.crate-countdown {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		font-variant-numeric: tabular-nums;
		color: var(--color-muted);
		white-space: nowrap;
	}

	@media (max-width: 379px) {
		.crate-countdown {
			display: none;
		}
	}

	.crate-ring-track {
		stroke: var(--dn-line);
	}

	/* transition matches the 250ms tick interval that updates it */
	.countdown-ring {
		stroke: var(--color-primary);
		transition: stroke-dashoffset 0.25s linear;
	}

	.crate-sweep {
		background: linear-gradient(
			115deg,
			transparent 28%,
			oklch(from var(--dn-action) l c h / 0.14) 50%,
			transparent 72%
		);
		opacity: 0;
		animation: crate-sweep 0.75s ease-out forwards;
	}

	.upnext-zone {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.625rem 0.875rem;
		border: 2px dashed var(--dn-edge);
		border-radius: 0.75rem;
	}
	.upnext-zone.is-over {
		border-color: var(--dn-action);
	}
	.upnext-zone__icon {
		display: grid;
		place-items: center;
		width: 2.25rem;
		height: 2.25rem;
		flex: none;
		border-radius: 9999px;
		background-color: var(--color-base-300);
		color: var(--color-muted);
		transition:
			background-color 150ms,
			color 150ms;
	}
	.upnext-zone.is-over .upnext-zone__icon {
		background-color: var(--dn-action);
		color: var(--dn-action-content);
	}
	.upnext-zone__title {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
</style>
