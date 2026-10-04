<script lang="ts">
	import type { CrateTrack, CrateReason, LocalAlbumSummary } from '$lib/types';
	import { flip } from 'svelte/animate';
	import { fly } from 'svelte/transition';
	import { crateCoverUrl } from '$lib/queries/local/LocalV3Adapters';
	import {
		Sparkles,
		RotateCcw,
		Dices,
		Clock,
		Play,
		ListPlus,
		GripVertical,
		RefreshCw,
		Disc3,
		ArrowDownToLine
	} from 'lucide-svelte';

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
		onQueueAlbum?: (a: LocalAlbumSummary) => void;
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

	const REASONS: Record<CrateReason, { label: string; icon: typeof Sparkles; badge: string }> = {
		recent: { label: 'Recently Added', icon: Sparkles, badge: 'badge-accent' },
		rediscover: { label: 'Rediscover', icon: RotateCcw, badge: 'badge-primary' },
		surprise: { label: 'Surprise', icon: Dices, badge: 'badge-warning' },
		same_era: { label: 'Same era', icon: Clock, badge: 'badge-info' }
	};
	const reasonMeta = (r: string) => REASONS[r as CrateReason] ?? REASONS.surprise;

	const flipDur = $derived(reducedMotion ? 0 : 340);
	let draggingId = $state<string | null>(null);
	let dragOverQueue = $state(false);

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

	function readAlbum(e: DragEvent): LocalAlbumSummary | null {
		const raw = e.dataTransfer?.getData('application/x-crate-album');
		if (!raw) return null;
		try {
			return JSON.parse(raw) as LocalAlbumSummary;
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

<section class="flex h-full flex-col gap-3">
	<header class="flex items-center justify-between px-1">
		<div class="flex items-center gap-2">
			<div class="now-playing-bars now-playing-bars--sm" aria-hidden="true">
				<span></span><span></span><span></span>
			</div>
			<h2 class="text-sm font-bold uppercase tracking-wider text-base-content/80">In the crate</h2>
		</div>
		<div class="relative grid h-7 w-7 place-items-center" title="Next mix loads automatically">
			<svg
				class="pointer-events-none absolute inset-0 h-full w-full -rotate-90"
				viewBox="0 0 36 36"
				aria-hidden="true"
			>
				<circle
					cx="18"
					cy="18"
					r="16"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					class="text-base-content/10"
				/>
				<circle
					cx="18"
					cy="18"
					r="16"
					fill="none"
					stroke="currentColor"
					stroke-width="2.5"
					stroke-linecap="round"
					class="countdown-ring text-accent"
					stroke-dasharray={RING_CIRCUMFERENCE}
					stroke-dashoffset={ringOffset}
				/>
			</svg>
			<button
				class="btn btn-circle btn-ghost btn-xs"
				onclick={onRefresh}
				aria-label="Shuffle the crate now"
				title="Shuffle the crate now"
			>
				<RefreshCw class="h-3.5 w-3.5 {isLoading ? 'animate-spin' : ''}" />
			</button>
		</div>
	</header>

	<div class="relative flex-1 overflow-hidden">
		<div bind:this={scrollEl} class="h-full space-y-2 overflow-y-auto pr-0.5">
			{#if tracks.length === 0 && isLoading}
				{#each Array(5) as _, i (i)}
					<div class="h-[4.25rem] animate-pulse rounded-xl bg-base-200/60"></div>
				{/each}
			{:else}
				{#each tracks as t, i (t.track_file_id)}
					{@const meta = reasonMeta(t.reason)}
					{@const Icon = meta.icon}
					{@const cover = crateCoverUrl(t)}
					<div
						class="crate-card group flex items-center gap-3 rounded-xl border border-base-content/5 bg-base-200/70 p-2.5 backdrop-blur-sm"
						class:is-dragging={draggingId === t.track_file_id}
						draggable="true"
						role="button"
						tabindex="0"
						ondragstart={(e) => onDragStart(e, t)}
						ondragend={() => (draggingId = null)}
						ondblclick={() => onPlay(t)}
						onkeydown={(e) => {
							if (e.key === 'Enter' || e.key === ' ') {
								e.preventDefault();
								onPlay(t);
							}
						}}
						animate:flip={{ duration: flipDur }}
						in:fly={{ y: 16, duration: reducedMotion ? 0 : 280, delay: reducedMotion ? 0 : i * 45 }}
						out:fly={{ y: -16, duration: reducedMotion ? 0 : 260 }}
					>
						<GripVertical
							class="h-4 w-4 shrink-0 cursor-grab text-base-content/25 group-hover:text-base-content/50"
						/>
						<div
							class="relative h-12 w-12 shrink-0 overflow-hidden rounded-md ring-1 ring-base-content/10"
						>
							{#if cover}
								<img
									src={cover}
									alt={t.album_name}
									class="h-full w-full object-cover"
									loading="lazy"
								/>
							{:else}
								<div class="flex h-full w-full items-center justify-center bg-base-300">
									<Disc3 class="h-5 w-5 text-base-content/30" />
								</div>
							{/if}
						</div>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm font-semibold text-base-content">{t.title}</p>
							<p class="truncate text-xs text-base-content/55">{t.artist_name}</p>
							<span class="badge badge-xs {meta.badge} mt-1 gap-1 border-none">
								<Icon class="h-2.5 w-2.5" />
								{meta.label}
							</span>
						</div>
						<div
							class="flex shrink-0 flex-col gap-1 opacity-0 transition-opacity group-hover:opacity-100"
						>
							<button
								class="btn btn-circle btn-ghost btn-xs"
								onclick={() => onPlay(t)}
								aria-label="Play now"
								title="Play now"
							>
								<Play class="h-3.5 w-3.5" />
							</button>
							<button
								class="btn btn-circle btn-ghost btn-xs"
								onclick={() => onQueue(t)}
								aria-label="Add to queue"
								title="Add to queue"
							>
								<ListPlus class="h-3.5 w-3.5" />
							</button>
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
		class="upnext-zone deck-droptarget flex items-center gap-3 overflow-hidden rounded-2xl border-2 border-dashed bg-gradient-to-r from-base-200/55 via-base-200/25 to-base-200/55 px-4 py-3 {dragOverQueue
			? 'border-accent/70'
			: 'border-base-content/15'}"
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
		<div
			class="grid h-9 w-9 shrink-0 place-items-center rounded-full transition-colors {dragOverQueue
				? 'bg-accent/20 text-accent'
				: 'bg-base-content/5 text-base-content/55'}"
		>
			<ArrowDownToLine class="h-4 w-4 {dragOverQueue || reducedMotion ? '' : 'animate-float'}" />
		</div>
		<div class="min-w-0 text-left">
			<p class="text-xs font-bold uppercase tracking-wider text-base-content/75">Up Next</p>
			<p class="truncate text-[11px] text-base-content/45">Drop a song or album to queue</p>
		</div>
		{#if upcomingCount > 0}
			<span
				class="badge badge-sm badge-accent ml-auto shrink-0 font-semibold"
				title="{upcomingCount} in the queue"
			>
				{upcomingCount}
			</span>
		{/if}
	</div>
</section>
