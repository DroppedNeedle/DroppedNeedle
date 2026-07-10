<script lang="ts">
	import { playerStore } from '$lib/stores/player.svelte';
	import { eqStore } from '$lib/stores/eq.svelte';
	import { scrobbleManager } from '$lib/stores/scrobble.svelte';
	import EqPanel from '$lib/components/EqPanel.svelte';
	import AudioQualityBadge from '$lib/components/AudioQualityBadge.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import type { CrateTrack, LocalAlbumSummary } from '$lib/types';
	import {
		Play,
		Pause,
		SkipBack,
		SkipForward,
		ChevronsLeft,
		ChevronsRight,
		Shuffle,
		Disc3,
		Sparkles,
		Dices,
		ListMusic,
		Check,
		CircleX
	} from 'lucide-svelte';

	interface Props {
		onDropPlay: (track: CrateTrack) => void;
		onDropAlbum: (album: LocalAlbumSummary) => void;
		onPlayAll: () => void;
		onShuffleAll: () => void;
		onSurprise: () => void;
		onOpenQueue: () => void;
	}

	let { onDropPlay, onDropAlbum, onPlayAll, onShuffleAll, onSurprise, onOpenQueue }: Props =
		$props();

	const np = $derived(playerStore.nowPlaying);
	const isLocal = $derived(np?.sourceType === 'local');
	const isPlaying = $derived(playerStore.isPlaying);
	const format = $derived(playerStore.currentQueueItem?.format ?? null);
	const isYouTube = $derived(np?.sourceType === 'youtube');

	let dragOver = $state(false);
	let eqPanelOpen = $state(false);

	function fmt(seconds: number): string {
		if (!Number.isFinite(seconds) || seconds < 0) return '0:00';
		const m = Math.floor(seconds / 60);
		const s = Math.floor(seconds % 60);
		return `${m}:${s.toString().padStart(2, '0')}`;
	}

	const progressPct = $derived(
		playerStore.duration > 0 ? (playerStore.progress / playerStore.duration) * 100 : 0
	);

	function onSeek(e: Event) {
		const target = e.currentTarget as HTMLInputElement;
		const ratio = Number(target.value) / 1000;
		if (playerStore.duration > 0) playerStore.seekTo(ratio * playerStore.duration);
	}

	/*
	 * Volume knob — a real rotary: −135°..+135° sweep mapped onto 0..100.
	 * Drag vertically (up = louder), spin the wheel, or use arrow keys.
	 * Same iOS Safari touch-guard trick as the EQ sliders: touch-action /
	 * user-select / touch-callout go INLINE so scroll and long-press callout
	 * cannot steal the vertical drag gesture.
	 */
	const TOUCH_GUARD_STYLE =
		'touch-action: none; -webkit-user-select: none; user-select: none; -webkit-touch-callout: none;';
	const VOL_DRAG_SENSITIVITY = 0.6;
	const volAngle = $derived(-135 + (playerStore.volume / 100) * 270);

	let volDragging = false;
	let volDragStartY = 0;
	let volDragStartVol = 0;

	function clampVol(v: number): number {
		return Math.max(0, Math.min(100, Math.round(v)));
	}

	function volPointerDown(e: PointerEvent) {
		volDragging = true;
		volDragStartY = e.clientY;
		volDragStartVol = playerStore.volume;
		const el = e.currentTarget as HTMLElement;
		el.focus();
		try {
			el.setPointerCapture(e.pointerId);
		} catch {
			// Some browsers (notably iOS Safari edge cases) refuse capture;
			// the drag still tracks via pointermove on the knob element.
		}
	}

	function volPointerMove(e: PointerEvent) {
		if (!volDragging) return;
		playerStore.setVolume(
			clampVol(volDragStartVol + (volDragStartY - e.clientY) * VOL_DRAG_SENSITIVITY)
		);
	}

	function volPointerUp() {
		volDragging = false;
	}

	function volWheel(e: WheelEvent) {
		e.preventDefault();
		playerStore.setVolume(clampVol(playerStore.volume + (e.deltaY < 0 ? 4 : -4)));
	}

	function volKeydown(e: KeyboardEvent) {
		let next: number | null = null;
		switch (e.key) {
			case 'ArrowUp':
			case 'ArrowRight':
				next = playerStore.volume + 5;
				break;
			case 'ArrowDown':
			case 'ArrowLeft':
				next = playerStore.volume - 5;
				break;
			case 'PageUp':
				next = playerStore.volume + 10;
				break;
			case 'PageDown':
				next = playerStore.volume - 10;
				break;
			case 'Home':
				next = 0;
				break;
			case 'End':
				next = 100;
				break;
		}
		if (next !== null) {
			e.preventDefault();
			// the app's global volume shortcut also listens for arrows — stop the
			// event here so a focused knob press doesn't double-step the volume
			e.stopPropagation();
			playerStore.setVolume(clampVol(next));
		}
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

	function handleDragOver(e: DragEvent) {
		const types = e.dataTransfer?.types;
		if (
			types?.includes('application/x-crate-track') ||
			types?.includes('application/x-crate-album')
		) {
			e.preventDefault();
			dragOver = true;
		}
	}

	/*
	 * Tonearm tracking — like a real deck, the arm rides the groove: it drops at
	 * the outer edge on track 1 and creeps toward the label as the album plays
	 * out, interpolating across (track position + progress within the track).
	 * When np flips null→set the same inline transition eases the arm from rest
	 * down onto the groove — the needle-drop for free.
	 *
	 * Geometry: the arm is mounted in its own lane on the RIGHT of the plinth,
	 * pivot at the top; at rest (0deg) it hangs vertically along the deck edge,
	 * clear of the vinyl. Positive rotation swings the stylus inward over the
	 * record's lower-right quadrant: ~0.95 of the groove band's outer radius at
	 * ARM_OUTER, down to the ~0.48R run-out by ARM_INNER.
	 */
	const ARM_REST = 0;
	const ARM_OUTER = 21;
	const ARM_INNER = 40;
	const trackProgress = $derived(
		playerStore.duration > 0 ? Math.min(1, playerStore.progress / playerStore.duration) : 0
	);
	const albumProgress = $derived(
		playerStore.queueLength > 0
			? Math.min(1, (playerStore.currentTrackNumber - 1 + trackProgress) / playerStore.queueLength)
			: 0
	);
	const armAngle = $derived(np ? ARM_OUTER + (ARM_INNER - ARM_OUTER) * albumProgress : ARM_REST);

	function handleDrop(e: DragEvent) {
		e.preventDefault();
		dragOver = false;
		const track = readTrack(e);
		if (track) {
			onDropPlay(track);
			return;
		}
		const album = readAlbum(e);
		if (album) onDropAlbum(album);
	}
</script>

<div
	class="deck-droptarget tt-chassis grain relative flex flex-col items-center gap-6 rounded-3xl p-6 sm:p-8"
	class:is-over={dragOver}
	role="region"
	aria-label="Now playing turntable - drop a track here to play it"
	ondragover={handleDragOver}
	ondragleave={() => (dragOver = false)}
	ondrop={handleDrop}
>
	<!-- platter sits left in the plinth; the tonearm gets its own lane on the right -->
	<div class="relative w-full max-w-[39rem] lg:max-w-[44rem]">
		<div class="relative aspect-square w-[82%]">
			<!-- recessed platter well cut into the plinth -->
			<div class="tt-well pointer-events-none absolute -inset-[2.75%] rounded-full"></div>
			<!-- Glow inset-0 must match the record's edge so the circles align. -->
			<div
				class="deck-halo absolute inset-0 -z-10 rounded-full"
				class:animate-glow-pulse={isPlaying}
			></div>

			<div
				class="turntable-platter vinyl-spin absolute inset-0 rounded-full"
				class:is-paused={!isPlaying}
			>
				{#if np}
					<!-- picture disc: the artwork IS the record, pressed edge to edge.
				     AlbumImage so the URL matches browse carousels and hits cache. -->
					<div class="absolute inset-[2.5%] overflow-hidden rounded-full">
						<AlbumImage
							mbid={np.albumId}
							remoteUrl={np.coverRemoteUrl ?? null}
							customUrl={np.coverUrl}
							alt={np.albumName ?? 'Album'}
							size="full"
							lazy={false}
							rounded="none"
							className="h-full w-full object-cover"
						/>
						<!-- pressed grooves + vinyl sheen over the art -->
						<div class="dn-vinyl-grooves pointer-events-none absolute inset-0 rounded-full"></div>
					</div>
				{:else}
					<div
						class="absolute inset-[33.5%] flex items-center justify-center rounded-full bg-base-300 ring-1 ring-base-content/25"
					>
						<Disc3 class="h-8 w-8 text-base-content/30" />
					</div>
				{/if}
				<div
					class="pointer-events-none absolute inset-[9%] rounded-full border border-base-100/25"
				></div>
				<div
					class="pointer-events-none absolute inset-[18%] rounded-full border border-base-100/20"
				></div>
				<div
					class="pointer-events-none absolute inset-[27%] rounded-full border border-base-100/15"
				></div>
				<!-- spindle + label cap -->
				<div
					class="absolute inset-[46%] rounded-full bg-base-100 shadow-[0_0_0_2px_oklch(from_var(--color-base-100)_l_c_h_/_0.55)] ring-1 ring-base-content/30"
				></div>
				<div
					class="absolute inset-[49.2%] rounded-full bg-base-300 ring-1 ring-base-content/40"
				></div>
			</div>
		</div>

		<!-- tonearm lane: pivot anchored top-right on the plinth body, arm
		     hanging down the deck's right edge. Rotation happens around the
		     pivot (origin 50% 13.4%); the counterweight rides above it, the
		     tube drops to the headshell whose stylus lands on the vinyl's
		     lower-right groove arc while playing. -->
		<div
			class="tonearm pointer-events-none absolute right-[6.75%] top-[2%] h-[88%] w-[6.5%]"
			class:is-playing={isPlaying}
			style="transform-origin: 50% 13.4%; transform: rotate({armAngle}deg); transition: transform 1.4s linear;"
		>
			<div
				class="tt-counterweight absolute left-1/2 top-[4%] h-[7%] w-3.5 -translate-x-1/2 rounded-[3px]"
			></div>
			<div
				class="tt-armtube absolute left-1/2 top-[13.4%] h-[75%] w-1.5 -translate-x-1/2 rounded-full"
			></div>
			<!-- machined pivot base + bearing cap -->
			<div
				class="tt-pivotbase absolute left-1/2 top-[13.4%] h-6 w-6 -translate-x-1/2 -translate-y-1/2 rounded-full"
			></div>
			<div
				class="tt-pivotcap absolute left-1/2 top-[13.4%] h-3 w-3 -translate-x-1/2 -translate-y-1/2 rounded-full"
			></div>
			<div
				class="tt-headshell absolute left-1/2 top-[87.5%] h-[5%] w-2.5 -translate-x-1/2 rotate-[14deg] rounded-[2px]"
			>
				<div
					class="tt-stylus absolute -bottom-[2px] left-[15%] h-[4px] w-[3px] rounded-[1px]"
				></div>
			</div>
		</div>
	</div>

	{#if np}
		<!-- pitch/strobe-style seek rail spanning under the platter -->
		<div class="flex w-full max-w-md items-center gap-3">
			<span class="tt-digits w-10 shrink-0 text-right text-xs tabular-nums"
				>{fmt(playerStore.progress)}</span
			>
			<input
				type="range"
				min="0"
				max="1000"
				value={Math.round(progressPct * 10)}
				oninput={onSeek}
				disabled={!playerStore.isSeekable}
				aria-label="Seek"
				class="tt-pitch min-w-0 flex-1"
			/>
			<span class="tt-digits w-10 shrink-0 text-xs tabular-nums">{fmt(playerStore.duration)}</span>
		</div>

		<!-- illuminated display panel -->
		<div
			class="tt-display flex w-full max-w-md flex-col items-center gap-1 rounded-2xl px-5 py-3 text-center"
		>
			<div class="flex w-full items-center justify-center gap-2">
				{#if isPlaying}
					<div class="now-playing-bars now-playing-bars--sm shrink-0">
						<span></span><span></span><span></span>
					</div>
				{/if}
				<p class="truncate text-lg font-bold text-base-content">{np.trackName ?? np.albumName}</p>
			</div>
			<p class="w-full truncate text-sm text-base-content/70">
				{np.artistName}{#if np.albumName}<span class="text-base-content/40">
						&middot; {np.albumName}</span
					>{/if}
			</p>
			{#if format || (scrobbleManager.enabled && scrobbleManager.status !== 'idle')}
				<!-- tiny status readout strip -->
				<div
					class="mt-1.5 flex w-full items-center justify-center gap-2 border-t border-base-content/10 pt-2"
				>
					{#if format}
						<AudioQualityBadge codec={format} />
					{/if}
					{#if scrobbleManager.enabled && scrobbleManager.status !== 'idle'}
						<div class="tooltip tooltip-top" data-tip={scrobbleManager.tooltip}>
							{#if scrobbleManager.status === 'scrobbled'}
								<Check class="h-4 w-4 text-success" />
							{:else if scrobbleManager.status === 'error'}
								<CircleX class="h-4 w-4 text-error" />
							{:else}
								<span class="badge badge-info badge-sm gap-1 font-semibold">
									<span class="status status-md status-info"></span>
									Tracking
								</span>
							{/if}
						</div>
					{/if}
				</div>
			{/if}
		</div>

		<!-- hardware control rail on the plinth -->
		<div class="flex w-full flex-wrap items-end justify-center gap-x-5 gap-y-4">
			<div class="flex items-end gap-2">
				<div class="flex flex-col items-center gap-1.5">
					<button
						class="tt-key tt-key--sm"
						onclick={() => playerStore.toggleShuffle()}
						aria-pressed={playerStore.shuffleEnabled}
						aria-label="Shuffle"
					>
						<Shuffle class="h-3.5 w-3.5 {playerStore.shuffleEnabled ? 'text-accent' : ''}" />
					</button>
					<span class="tt-cap flex items-center gap-1">
						<span class="tt-led" class:is-on={playerStore.shuffleEnabled}></span>Shfl
					</span>
				</div>
				<div class="flex flex-col items-center gap-1.5">
					<div class="tooltip tooltip-top" data-tip="Previous album">
						<button
							class="tt-key tt-key--sm"
							onclick={() => playerStore.previousAlbum()}
							disabled={!playerStore.hasPreviousAlbum}
							aria-label="Previous album"
						>
							<ChevronsLeft class="h-4 w-4" />
						</button>
					</div>
					<span class="tt-cap">Alb</span>
				</div>
				<div class="flex flex-col items-center gap-1.5">
					<button
						class="tt-key"
						onclick={() => playerStore.previousTrack()}
						disabled={!playerStore.hasPrevious}
						aria-label="Previous"
					>
						<SkipBack class="h-4.5 w-4.5" />
					</button>
					<span class="tt-cap">Trk</span>
				</div>
				<div class="flex flex-col items-center gap-1.5">
					<button
						class="tt-power"
						class:is-error={playerStore.playbackState === 'error'}
						onclick={() => playerStore.togglePlay()}
						aria-label={isPlaying ? 'Pause' : 'Play'}
					>
						{#if isPlaying}<Pause class="h-7 w-7" />{:else}<Play
								class="h-7 w-7 translate-x-[1px]"
							/>{/if}
					</button>
					<span class="tt-cap">33&#8531;</span>
				</div>
				<div class="flex flex-col items-center gap-1.5">
					<button
						class="tt-key"
						onclick={() => playerStore.nextTrack()}
						disabled={!playerStore.hasNext}
						aria-label="Next"
					>
						<SkipForward class="h-4.5 w-4.5" />
					</button>
					<span class="tt-cap">Trk</span>
				</div>
				<div class="flex flex-col items-center gap-1.5">
					<div class="tooltip tooltip-top" data-tip="Next album">
						<button
							class="tt-key tt-key--sm"
							onclick={() => playerStore.nextAlbum()}
							disabled={!playerStore.hasNextAlbum}
							aria-label="Next album"
						>
							<ChevronsRight class="h-4 w-4" />
						</button>
					</div>
					<span class="tt-cap">Alb</span>
				</div>
			</div>

			<div class="flex items-end gap-4">
				<div class="flex flex-col items-center gap-1.5">
					<div
						class="tt-knob"
						role="slider"
						tabindex="0"
						aria-label="Volume"
						aria-orientation="vertical"
						aria-valuemin="0"
						aria-valuemax="100"
						aria-valuenow={playerStore.volume}
						aria-valuetext="{playerStore.volume}%"
						style={TOUCH_GUARD_STYLE}
						onpointerdown={volPointerDown}
						onpointermove={volPointerMove}
						onpointerup={volPointerUp}
						onpointercancel={volPointerUp}
						onwheel={volWheel}
						onkeydown={volKeydown}
					>
						<span class="tt-knob__cap" style="transform: rotate({volAngle}deg)">
							<span class="tt-knob__line"></span>
						</span>
					</div>
					<span class="tt-cap">Vol</span>
				</div>
				<div class="flex flex-col items-center gap-1.5">
					<div
						class="tooltip tooltip-top"
						data-tip={isYouTube ? 'EQ unavailable for YouTube' : 'Equalizer'}
					>
						<button
							class="tt-knob tt-knob--btn"
							onclick={() => (eqPanelOpen = !eqPanelOpen)}
							disabled={isYouTube}
							aria-expanded={eqPanelOpen}
							aria-label="Toggle equalizer"
						>
							<span class="tt-knob__cap">
								<span class="tt-knob__line"></span>
							</span>
						</button>
					</div>
					<span class="tt-cap flex items-center gap-1">
						<span class="tt-led" class:is-on={eqStore.enabled && !isYouTube}></span>EQ
					</span>
				</div>
				<div class="flex flex-col items-center gap-1.5">
					<div class="indicator">
						{#if playerStore.upcomingQueueLength > 0}
							<span class="badge indicator-item badge-xs badge-accent"
								>{playerStore.upcomingQueueLength}</span
							>
						{/if}
						<button class="tt-queue" onclick={onOpenQueue} aria-label="Open queue">
							<ListMusic class="h-4.5 w-4.5" />
						</button>
					</div>
					<span class="tt-cap">Queue</span>
				</div>
			</div>
		</div>

		{#if isLocal}
			<p class="text-[11px] uppercase tracking-wider text-base-content/30">
				drag a record onto the deck
			</p>
		{/if}
	{:else}
		<div class="flex w-full max-w-md flex-col items-center gap-4 text-center">
			<div class="tt-display flex w-full flex-col items-center gap-1 rounded-2xl px-5 py-4">
				<p class="text-lg font-bold text-base-content">Drop the needle</p>
				<p class="text-sm text-base-content/60">
					Drag a record from the crate onto the deck, or just hit play.
				</p>
			</div>
			<div class="flex flex-wrap items-center justify-center gap-2">
				<button class="btn btn-primary btn-sm gap-2" onclick={onPlayAll}>
					<Play class="h-4 w-4" /> Play All
				</button>
				<button class="btn btn-ghost btn-sm gap-2" onclick={onShuffleAll}>
					<Shuffle class="h-4 w-4" /> Shuffle
				</button>
				<button class="btn btn-ghost btn-sm gap-2" onclick={onSurprise}>
					<Dices class="h-4 w-4" /> Surprise me
				</button>
			</div>
			<div
				class="flex items-center gap-1.5 text-[11px] uppercase tracking-wider text-base-content/30"
			>
				<Sparkles class="h-3 w-3" /> your library, ready to spin
			</div>
		</div>
	{/if}
</div>

<EqPanel bind:open={eqPanelOpen} onclose={() => (eqPanelOpen = false)} />

<style>
	/* ── deck chassis: layered charcoal plinth with brushed texture ─────── */
	.tt-chassis {
		border: 1px solid oklch(from var(--color-base-content) l c h / 0.07);
		background:
			repeating-linear-gradient(
				90deg,
				oklch(1 0 0 / 0.014) 0px,
				oklch(1 0 0 / 0.014) 1px,
				transparent 1px,
				transparent 3px
			),
			repeating-linear-gradient(
				180deg,
				transparent 0px,
				transparent 5px,
				oklch(0 0 0 / 0.045) 5px,
				oklch(0 0 0 / 0.045) 6px
			),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-200) calc(l + 0.025) c h) 0%,
				var(--color-base-200) 45%,
				oklch(from var(--color-base-200) calc(l - 0.02) c h) 100%
			);
		/* hairline top light + body shadow + soft "feet" shadow */
		box-shadow:
			inset 0 1px 0 oklch(1 0 0 / 0.07),
			inset 0 -1px 0 oklch(0 0 0 / 0.3),
			0 22px 44px -20px oklch(0 0 0 / 0.6),
			0 40px 30px -32px oklch(0 0 0 / 0.55);
		--grain-opacity: 0.05;
	}
	/* re-state the drop-target ring locally so it wins over the chassis shadow */
	.tt-chassis.is-over {
		box-shadow:
			0 0 0 3px oklch(from var(--color-accent) l c h / 0.55),
			0 0 40px oklch(from var(--color-accent) l c h / 0.35),
			inset 0 1px 0 oklch(1 0 0 / 0.07),
			0 22px 44px -20px oklch(0 0 0 / 0.6);
	}

	/* recessed well the platter sits in */
	.tt-well {
		background: radial-gradient(circle at 50% 42%, oklch(0 0 0 / 0.32), oklch(0 0 0 / 0.18) 72%);
		box-shadow:
			inset 0 3px 10px oklch(0 0 0 / 0.55),
			inset 0 -1px 0 oklch(1 0 0 / 0.06),
			0 1px 0 oklch(1 0 0 / 0.05);
	}

	/* ── tonearm anatomy ─────────────────────────────────────────────────── */
	/* the arm hangs vertically, so the metal sheen runs across the tube (90deg) */
	.tt-armtube {
		background: linear-gradient(
			90deg,
			oklch(from var(--color-base-content) l c h / 0.5),
			oklch(from var(--color-base-content) l c h / 0.22)
		);
		box-shadow: 1px 0 2px oklch(0 0 0 / 0.4);
	}
	.tt-counterweight {
		background: linear-gradient(
			90deg,
			oklch(from var(--color-base-content) l c h / 0.55),
			oklch(from var(--color-base-content) l c h / 0.25)
		);
		box-shadow:
			inset 1px 0 0 oklch(1 0 0 / 0.15),
			1px 1px 2px oklch(0 0 0 / 0.45);
	}
	.tt-headshell {
		background: linear-gradient(
			90deg,
			oklch(from var(--color-base-content) l c h / 0.6),
			oklch(from var(--color-base-content) l c h / 0.3)
		);
		/* soft contact shadow cast onto the vinyl so the cartridge reads as
		   sitting ON the record rather than floating beside it */
		filter: drop-shadow(0 3px 3px oklch(0 0 0 / 0.55));
		box-shadow: 0 1px 2px oklch(0 0 0 / 0.45);
	}
	.tt-stylus {
		background: var(--color-accent);
		box-shadow: 0 0 3px oklch(from var(--color-accent) l c h / 0.7);
	}
	.tt-pivotbase {
		background:
			radial-gradient(circle at 34% 30%, oklch(1 0 0 / 0.14), transparent 55%),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-300) calc(l + 0.04) c h),
				oklch(from var(--color-base-300) calc(l - 0.03) c h)
			);
		box-shadow:
			inset 0 1px 0 oklch(1 0 0 / 0.12),
			inset 0 0 0 1px oklch(0 0 0 / 0.35),
			0 2px 4px oklch(0 0 0 / 0.4);
	}
	.tt-pivotcap {
		background: radial-gradient(
			circle at 40% 35%,
			oklch(from var(--color-base-content) l c h / 0.5),
			oklch(from var(--color-base-content) l c h / 0.15)
		);
		box-shadow: inset 0 0 0 1px oklch(0 0 0 / 0.3);
	}

	/* ── illuminated display panel ───────────────────────────────────────── */
	.tt-display {
		background: linear-gradient(180deg, oklch(0 0 0 / 0.34), oklch(0 0 0 / 0.2));
		border: 1px solid oklch(1 0 0 / 0.05);
		box-shadow:
			inset 0 2px 8px oklch(0 0 0 / 0.4),
			inset 0 -1px 0 oklch(1 0 0 / 0.04),
			0 1px 0 oklch(1 0 0 / 0.05);
	}
	.tt-digits {
		font-family: var(--font-mono);
		color: oklch(from var(--color-base-content) l c h / 0.6);
	}

	/* ── mono captions + status LEDs ─────────────────────────────────────── */
	.tt-cap {
		font-family: var(--font-mono);
		font-size: 0.55rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.4);
		user-select: none;
	}
	.tt-led {
		width: 0.28rem;
		height: 0.28rem;
		border-radius: 999px;
		background: oklch(from var(--color-base-content) l c h / 0.18);
		box-shadow: inset 0 1px 1px oklch(0 0 0 / 0.5);
	}
	.tt-led.is-on {
		background: var(--color-accent);
		box-shadow: 0 0 6px oklch(from var(--color-accent) l c h / 0.85);
	}

	/* ── machined hardware buttons ───────────────────────────────────────── */
	.tt-key,
	.tt-power,
	.tt-queue {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		color: oklch(from var(--color-base-content) l c h / 0.72);
		background:
			radial-gradient(circle at 32% 28%, oklch(1 0 0 / 0.09), transparent 55%),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-300) calc(l + 0.035) c h),
				oklch(from var(--color-base-300) calc(l - 0.02) c h)
			);
		border: 1px solid oklch(0 0 0 / 0.4);
		box-shadow:
			inset 0 1px 0 oklch(1 0 0 / 0.1),
			inset 0 -2px 3px oklch(0 0 0 / 0.28),
			0 2px 4px oklch(0 0 0 / 0.35);
		transition:
			transform 0.08s ease,
			box-shadow 0.08s ease,
			color 0.15s ease;
	}
	.tt-key:hover:not(:disabled),
	.tt-power:hover:not(:disabled),
	.tt-queue:hover:not(:disabled) {
		color: oklch(from var(--color-base-content) l c h / 0.95);
	}
	.tt-key:active:not(:disabled),
	.tt-power:active:not(:disabled),
	.tt-queue:active:not(:disabled) {
		transform: translateY(1px);
		box-shadow:
			inset 0 2px 5px oklch(0 0 0 / 0.45),
			inset 0 1px 0 oklch(0 0 0 / 0.2);
	}
	.tt-key:disabled {
		opacity: 0.35;
		cursor: default;
	}
	.tt-key {
		width: 2.6rem;
		height: 2.6rem;
		border-radius: 999px;
	}
	.tt-key--sm {
		width: 2.15rem;
		height: 2.15rem;
	}
	.tt-power {
		width: 4.4rem;
		height: 4.4rem;
		border-radius: 999px;
		color: oklch(from var(--color-base-content) l c h / 0.9);
		box-shadow:
			inset 0 1px 0 oklch(1 0 0 / 0.12),
			inset 0 -3px 5px oklch(0 0 0 / 0.32),
			inset 0 0 0 4px oklch(0 0 0 / 0.14),
			0 3px 6px oklch(0 0 0 / 0.4);
	}
	.tt-power.is-error {
		color: var(--color-error);
		box-shadow:
			0 0 0 2px oklch(from var(--color-error) l c h / 0.5),
			inset 0 1px 0 oklch(1 0 0 / 0.12),
			inset 0 -3px 5px oklch(0 0 0 / 0.32),
			0 3px 6px oklch(0 0 0 / 0.4);
	}
	.tt-queue {
		width: 2.6rem;
		height: 2.6rem;
		border-radius: 0.6rem;
	}

	/* ── rotary knobs (volume drives rotation inline; EQ nudges on hover) ── */
	.tt-knob {
		position: relative;
		width: 3.1rem;
		height: 3.1rem;
		padding: 0;
		border: 0;
		border-radius: 999px;
		background:
			radial-gradient(circle at 50% 30%, oklch(1 0 0 / 0.05), transparent 60%), oklch(0 0 0 / 0.35);
		box-shadow:
			inset 0 2px 5px oklch(0 0 0 / 0.5),
			0 1px 0 oklch(1 0 0 / 0.05);
		cursor: grab;
	}
	.tt-knob:active {
		cursor: grabbing;
	}
	.tt-knob--btn {
		cursor: pointer;
	}
	.tt-knob--btn:disabled {
		opacity: 0.35;
		cursor: default;
	}
	.tt-knob__cap {
		position: absolute;
		inset: 4px;
		display: block;
		border-radius: 999px;
		background:
			radial-gradient(circle at 34% 28%, oklch(1 0 0 / 0.13), transparent 52%),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-300) calc(l + 0.045) c h),
				oklch(from var(--color-base-300) calc(l - 0.03) c h)
			);
		box-shadow:
			inset 0 1px 0 oklch(1 0 0 / 0.12),
			inset 0 -2px 4px oklch(0 0 0 / 0.32),
			0 2px 5px oklch(0 0 0 / 0.4);
	}
	@media (prefers-reduced-motion: no-preference) {
		.tt-knob__cap {
			transition: transform 0.12s ease;
		}
		.tt-knob--btn:hover:not(:disabled) .tt-knob__cap {
			transform: rotate(14deg);
		}
	}
	.tt-knob__line {
		position: absolute;
		left: 50%;
		top: 9%;
		width: 2px;
		height: 27%;
		margin-left: -1px;
		border-radius: 2px;
		background: var(--color-accent);
		box-shadow: 0 0 5px oklch(from var(--color-accent) l c h / 0.55);
	}

	/* ── pitch/strobe-styled seek slider ─────────────────────────────────── */
	.tt-pitch {
		-webkit-appearance: none;
		appearance: none;
		height: 1.4rem;
		background: transparent;
		cursor: pointer;
	}
	.tt-pitch:disabled {
		opacity: 0.4;
		cursor: default;
	}
	.tt-pitch::-webkit-slider-runnable-track {
		height: 5px;
		border-radius: 3px;
		background: linear-gradient(180deg, oklch(0 0 0 / 0.5), oklch(0 0 0 / 0.3));
		box-shadow:
			inset 0 1px 2px oklch(0 0 0 / 0.6),
			0 1px 0 oklch(1 0 0 / 0.05);
	}
	.tt-pitch::-webkit-slider-thumb {
		-webkit-appearance: none;
		appearance: none;
		width: 0.85rem;
		height: 1.35rem;
		margin-top: -0.52rem;
		border-radius: 3px;
		background:
			linear-gradient(
				90deg,
				transparent calc(50% - 1px),
				var(--color-accent) calc(50% - 1px),
				var(--color-accent) calc(50% + 1px),
				transparent calc(50% + 1px)
			),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-300) calc(l + 0.08) c h),
				oklch(from var(--color-base-300) calc(l - 0.02) c h)
			);
		border: 1px solid oklch(0 0 0 / 0.45);
		box-shadow:
			inset 0 1px 0 oklch(1 0 0 / 0.15),
			0 2px 4px oklch(0 0 0 / 0.5);
	}
	.tt-pitch::-moz-range-track {
		height: 5px;
		border-radius: 3px;
		background: linear-gradient(180deg, oklch(0 0 0 / 0.5), oklch(0 0 0 / 0.3));
		box-shadow:
			inset 0 1px 2px oklch(0 0 0 / 0.6),
			0 1px 0 oklch(1 0 0 / 0.05);
	}
	.tt-pitch::-moz-range-thumb {
		width: 0.85rem;
		height: 1.35rem;
		border-radius: 3px;
		background:
			linear-gradient(
				90deg,
				transparent calc(50% - 1px),
				var(--color-accent) calc(50% - 1px),
				var(--color-accent) calc(50% + 1px),
				transparent calc(50% + 1px)
			),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-300) calc(l + 0.08) c h),
				oklch(from var(--color-base-300) calc(l - 0.02) c h)
			);
		border: 1px solid oklch(0 0 0 / 0.45);
		box-shadow:
			inset 0 1px 0 oklch(1 0 0 / 0.15),
			0 2px 4px oklch(0 0 0 / 0.5);
	}
</style>
