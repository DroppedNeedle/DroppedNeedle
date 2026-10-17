<script lang="ts">
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
	import { X, RotateCcw } from 'lucide-svelte';
	import { fly } from '$lib/ui/motion';
	import IconButton from '$lib/ui/IconButton.svelte';

	let { open = $bindable(), onclose }: { open: boolean; onclose: () => void } = $props();

	const isYouTube = $derived(playerStore.nowPlaying?.sourceType === 'youtube');

	const TRACK_HEIGHT = 160;
	const GAIN_RANGE = EQ_MAX_GAIN - EQ_MIN_GAIN;
	const DB_TICKS = [12, 6, 0, -6, -12];

	let draggingIndex = $state<number | null>(null);
	let trackRefs: HTMLDivElement[] = [];

	function gainToY(gain: number): number {
		return ((EQ_MAX_GAIN - gain) / GAIN_RANGE) * TRACK_HEIGHT;
	}

	function yToGain(y: number): number {
		const clamped = Math.max(0, Math.min(TRACK_HEIGHT, y));
		const raw = EQ_MAX_GAIN - (clamped / TRACK_HEIGHT) * GAIN_RANGE;
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

	function handlePointerDown(index: number, e: PointerEvent): void {
		if (isYouTube || !eqStore.enabled) return;
		e.preventDefault();
		draggingIndex = index;
		(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
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

	function handlePresetChange(e: Event): void {
		const value = (e.target as HTMLSelectElement).value;
		if (value) {
			eqStore.applyPreset(value as EqPresetName);
		}
	}

	function handleClose(): void {
		open = false;
		onclose();
	}

	// Arrow keys step a band by half a decibel, Home and End jump to the limits.
	function handleBandKeydown(index: number, e: KeyboardEvent): void {
		if (isYouTube || !eqStore.enabled) return;
		const steps: Record<string, number> = {
			ArrowUp: 0.5,
			ArrowRight: 0.5,
			ArrowDown: -0.5,
			ArrowLeft: -0.5
		};
		let gain: number;
		if (e.key in steps) gain = eqStore.gains[index] + steps[e.key];
		else if (e.key === 'Home') gain = EQ_MIN_GAIN;
		else if (e.key === 'End') gain = EQ_MAX_GAIN;
		else return;
		e.preventDefault();
		e.stopPropagation();
		eqStore.setBandGain(index, Math.max(EQ_MIN_GAIN, Math.min(EQ_MAX_GAIN, gain)));
	}

	function handleKeydown(e: KeyboardEvent): void {
		if (e.key === 'Escape') {
			handleClose();
		}
	}
</script>

{#if open}
	<button
		class="fixed inset-0 z-60 bg-transparent"
		onclick={handleClose}
		aria-label="Close equalizer"
		tabindex="-1"
	></button>

	<div
		class="fixed bottom-24.5 right-4 z-70 w-120 max-w-[calc(100vw-2rem)]
			   rounded-box bg-base-300 shadow-[0_-8px_40px_rgba(0,0,0,0.45)] border border-base-content/5"
		role="dialog"
		aria-label="Equalizer"
		tabindex="-1"
		transition:fly={{ y: 20, duration: 200 }}
		onkeydown={handleKeydown}
	>
		<div class="flex items-center justify-between px-4 pt-3 pb-2">
			<div class="flex items-center gap-3">
				<h3 class="text-sm font-bold tracking-wide uppercase opacity-80">Equalizer</h3>
				<input
					type="checkbox"
					class="toggle toggle-sm toggle-primary"
					checked={eqStore.enabled}
					disabled={isYouTube}
					onchange={() => eqStore.toggleEq()}
					aria-label="Toggle equalizer"
				/>
			</div>
			<IconButton size="xs" icon={X} label="Close equalizer" onclick={handleClose} />
		</div>

		{#if isYouTube}
			<div class="mx-4 mb-3 rounded-lg bg-warning/10 border border-warning/20 px-3 py-2">
				<p class="text-xs text-warning">EQ is not available during YouTube playback</p>
			</div>
		{/if}

		<div class="flex items-center gap-2 px-4 pb-3">
			<select
				class="select select-sm flex-1"
				aria-label="Equalizer preset"
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
			<IconButton
				size="sm"
				icon={RotateCcw}
				label="Reset equalizer to flat"
				disabled={isYouTube || !eqStore.enabled}
				onclick={() => eqStore.resetToFlat()}
			/>
		</div>

		<div
			class="px-4 pb-4 pt-1 transition-opacity duration-200"
			class:opacity-30={isYouTube || !eqStore.enabled}
			class:pointer-events-none={isYouTube || !eqStore.enabled}
		>
			<div class="flex">
				<div
					class="flex flex-col justify-between pr-2 select-none"
					style="height: {TRACK_HEIGHT}px;"
				>
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
								class="relative w-full cursor-pointer touch-none"
								style="height: {TRACK_HEIGHT}px;"
								bind:this={trackRefs[i]}
								onpointerdown={(e) => handlePointerDown(i, e)}
								onpointermove={(e) => handlePointerMove(i, e)}
								onpointerup={handlePointerUp}
								onpointercancel={handlePointerUp}
								onkeydown={(e) => handleBandKeydown(i, e)}
								role="slider"
								tabindex={isYouTube || !eqStore.enabled ? -1 : 0}
								aria-label="{EQ_FREQUENCY_LABELS[i]} Hz"
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
	</div>
{/if}
