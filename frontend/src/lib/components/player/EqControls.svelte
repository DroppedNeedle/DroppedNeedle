<script lang="ts">
	/*
	 * EqControls — the equalizer body shared by the EqPanel and the Stage.
	 * Enable toggle, preset select, reset, and 10 draggable band sliders.
	 */
	import { eqStore } from '$lib/stores/eq.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import {
		EQ_FREQUENCY_LABELS,
		EQ_BAND_COUNT,
		EQ_MIN_GAIN,
		EQ_MAX_GAIN,
		EQ_PRESET_NAMES,
		type EqPresetName
	} from '$lib/stores/eqPresets';
	import { RotateCcw } from 'lucide-svelte';

	interface Props {
		trackHeight?: number;
	}

	let { trackHeight = 160 }: Props = $props();

	const isYouTube = $derived(playerStore.nowPlaying?.sourceType === 'youtube');

	const GAIN_RANGE = EQ_MAX_GAIN - EQ_MIN_GAIN;
	const DB_TICKS = [12, 6, 0, -6, -12];

	/*
	 * iOS Safari/PWA drag robustness: the tracks live inside scrollable
	 * ancestors, so touch-action/user-select/touch-callout are set INLINE
	 * (classes can lose specificity battles and Safari honours the inline
	 * style earliest) to stop scroll, text-selection and long-press callout
	 * from stealing the vertical drag gesture.
	 */
	const TOUCH_GUARD_STYLE =
		'touch-action: none; -webkit-user-select: none; user-select: none; -webkit-touch-callout: none;';

	let draggingIndex = $state<number | null>(null);
	let trackRefs: HTMLDivElement[] = [];

	function gainToY(gain: number): number {
		return ((EQ_MAX_GAIN - gain) / GAIN_RANGE) * trackHeight;
	}

	function yToGain(y: number): number {
		const clamped = Math.max(0, Math.min(trackHeight, y));
		const raw = EQ_MAX_GAIN - (clamped / trackHeight) * GAIN_RANGE;
		return Math.round(raw * 2) / 2;
	}

	function barStyle(gain: number): { top: string; height: string } {
		const center = gainToY(0);
		const pos = gainToY(gain);
		if (gain >= 0) {
			return { top: `${pos}px`, height: `${center - pos}px` };
		}
		return { top: `${center}px`, height: `${pos - center}px` };
	}

	/** Enables the EQ when a band is touched while off (YouTube stays hard-blocked). */
	function ensureEnabled(): boolean {
		if (isYouTube) return false;
		if (!eqStore.enabled) eqStore.toggleEq();
		return true;
	}

	function handlePointerDown(index: number, e: PointerEvent): void {
		if (!ensureEnabled()) return;
		e.preventDefault();
		draggingIndex = index;
		try {
			(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
		} catch {
			// Some browsers (notably iOS Safari edge cases) refuse capture;
			// the drag still tracks via pointermove on the track element.
		}
		updateGainFromPointer(index, e);
	}

	function handlePointerMove(index: number, e: PointerEvent): void {
		if (draggingIndex !== index) return;
		updateGainFromPointer(index, e);
	}

	function handlePointerUp(): void {
		draggingIndex = null;
	}

	function updateGainFromPointer(index: number, e: PointerEvent): void {
		const track = trackRefs[index];
		if (!track) return;
		const rect = track.getBoundingClientRect();
		const y = e.clientY - rect.top;
		eqStore.setBandGain(index, yToGain(y));
	}

	function handleTrackKeydown(index: number, e: KeyboardEvent): void {
		let next: number;
		switch (e.key) {
			case 'ArrowUp':
				next = eqStore.gains[index] + 0.5;
				break;
			case 'ArrowDown':
				next = eqStore.gains[index] - 0.5;
				break;
			case 'Home':
				next = EQ_MIN_GAIN;
				break;
			case 'End':
				next = EQ_MAX_GAIN;
				break;
			default:
				return;
		}
		e.preventDefault();
		if (!ensureEnabled()) return;
		eqStore.setBandGain(index, next);
	}

	function handlePresetChange(e: Event): void {
		const value = (e.target as HTMLSelectElement).value;
		if (value) {
			eqStore.applyPreset(value as EqPresetName);
		}
	}
</script>

<div class="flex items-center gap-3 pb-2">
	<!-- self-contained switch: DaisyUI's .toggle collapses to ~5px in this build,
	     so the enable control is drawn explicitly to stay visible/tappable -->
	<button
		type="button"
		role="switch"
		aria-checked={eqStore.enabled}
		aria-label="Toggle equalizer"
		disabled={isYouTube}
		onclick={() => eqStore.toggleEq()}
		class="relative h-5.5 w-10 shrink-0 cursor-pointer rounded-full border transition-colors duration-150 disabled:cursor-not-allowed disabled:opacity-40 {eqStore.enabled
			? 'border-accent bg-accent'
			: 'border-base-content/25 bg-base-content/10'}"
	>
		<span
			class="absolute top-1/2 h-4 w-4 -translate-y-1/2 rounded-full shadow transition-[left] duration-150 {eqStore.enabled
				? 'left-[calc(100%-1.125rem)] bg-base-100'
				: 'left-0.5 bg-base-content/60'}"
		></span>
	</button>
	<div class="flex min-w-0 flex-1 flex-col">
		<span class="font-mono text-[0.62rem] font-bold uppercase tracking-[0.2em] text-base-content/70"
			>Equalizer</span
		>
		<span class="text-[11px] leading-tight text-base-content/50">
			{#if isYouTube}
				Unavailable during YouTube playback
			{:else if eqStore.enabled}
				On
			{:else}
				Off — tap a band or flip to enable
			{/if}
		</span>
	</div>
</div>

<div class="flex items-center gap-2 pb-3">
	<select
		class="select select-sm select-bordered flex-1 rounded-full text-xs"
		value={eqStore.activePreset ?? ''}
		onchange={handlePresetChange}
		disabled={isYouTube || !eqStore.enabled}
	>
		{#if eqStore.activePreset === null}
			<option value="" disabled>Custom</option>
		{/if}
		{#each EQ_PRESET_NAMES as name (name)}
			<option value={name}>{name}</option>
		{/each}
	</select>
	<div class="tooltip tooltip-left" data-tip="Reset to flat">
		<button
			class="btn btn-ghost btn-sm btn-circle"
			onclick={() => eqStore.resetToFlat()}
			disabled={isYouTube || !eqStore.enabled}
			aria-label="Reset equalizer to flat"
		>
			<RotateCcw class="h-3.5 w-3.5" />
		</button>
	</div>
</div>

{#if isYouTube}
	<div class="mb-3 rounded-lg bg-warning/10 border border-warning/20 px-3 py-2">
		<p class="text-xs text-warning">EQ is not available during YouTube playback</p>
	</div>
{/if}

<div
	class="pt-1 transition-opacity duration-200"
	class:opacity-30={isYouTube}
	class:pointer-events-none={isYouTube}
	class:opacity-40={!isYouTube && !eqStore.enabled}
	style={TOUCH_GUARD_STYLE}
>
	<div class="flex">
		<div class="flex flex-col justify-between pr-2 select-none" style="height: {trackHeight}px;">
			{#each DB_TICKS as tick (tick)}
				<span class="text-[9px] tabular-nums opacity-40 leading-none text-right w-5">
					{tick > 0 ? '+' : ''}{tick}
				</span>
			{/each}
		</div>

		<div class="flex flex-1 gap-0">
			{#each { length: EQ_BAND_COUNT } as _, i (i)}
				<div class="flex flex-col items-center flex-1 min-w-7">
					<span
						class="text-[10px] tabular-nums font-semibold mb-1.5 select-none h-3 leading-none"
						style="color: oklch(from var(--color-accent) l c h / {Math.min(
							1,
							0.5 + (Math.abs(eqStore.gains[i]) / EQ_MAX_GAIN) * 0.5
						)})"
					>
						{eqStore.gains[i] > 0 ? '+' : ''}{eqStore.gains[i].toFixed(
							eqStore.gains[i] % 1 === 0 ? 0 : 1
						)}
					</span>

					<div
						class="relative w-full cursor-pointer rounded-sm focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
						style="height: {trackHeight}px; {TOUCH_GUARD_STYLE}"
						bind:this={trackRefs[i]}
						onpointerdown={(e) => handlePointerDown(i, e)}
						onpointermove={(e) => handlePointerMove(i, e)}
						onpointerup={handlePointerUp}
						onpointercancel={handlePointerUp}
						onkeydown={(e) => handleTrackKeydown(i, e)}
						role="slider"
						tabindex="0"
						aria-label="{EQ_FREQUENCY_LABELS[i]} Hz"
						aria-orientation="vertical"
						aria-valuemin={EQ_MIN_GAIN}
						aria-valuemax={EQ_MAX_GAIN}
						aria-valuenow={eqStore.gains[i]}
						aria-valuetext="{eqStore.gains[i]} dB"
					>
						<div
							class="absolute left-1/2 -translate-x-1/2 w-0.75 h-full rounded-full bg-base-content/8"
						></div>

						<div
							class="absolute left-0 right-0 h-px bg-base-content/15"
							style="top: {gainToY(0)}px;"
						></div>

						<div
							class="absolute left-1/2 -translate-x-1/2 w-1.75 rounded-full transition-[height,top] duration-75"
							style="top: {barStyle(eqStore.gains[i]).top}; height: {barStyle(eqStore.gains[i])
								.height}; background: oklch(from var(--color-accent) l c h / 0.7);"
						></div>

						<div
							class="absolute left-1/2 -translate-x-1/2 w-3.5 h-3.5 rounded-full
								   border-2 transition-transform duration-75
								   {draggingIndex === i ? 'scale-125' : 'hover:scale-110'}"
							style="top: {gainToY(eqStore.gains[i]) - 7}px;
								   background: var(--color-accent);
								   border-color: oklch(from var(--color-accent) l c h / 0.5);
								   box-shadow: 0 0 8px oklch(from var(--color-accent) l c h / 0.35);"
						></div>
					</div>

					<span class="text-[9px] opacity-50 mt-1.5 select-none">{EQ_FREQUENCY_LABELS[i]}</span>
				</div>
			{/each}
		</div>
	</div>
</div>
