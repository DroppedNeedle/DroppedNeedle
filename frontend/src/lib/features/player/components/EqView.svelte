<!--
	The ten-band equalizer, for the drawer's Equalizer tab and the Listening
	Room's panel: the on/off switch, a preset, a reset to flat, and one
	vertical slider per band (pointer drag, or the arrow keys by half a
	decibel, Home and End to the limits). Nothing works while it is off or
	a YouTube track plays.
-->
<script lang="ts">
	import { eqStore } from '$lib/features/player/state/eq.svelte';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import {
		EQ_FREQUENCY_LABELS,
		EQ_BAND_COUNT,
		EQ_MIN_GAIN,
		EQ_MAX_GAIN,
		EQ_PRESET_NAMES,
		type EqPresetName
	} from '$lib/features/player/state/eqPresets';
	import { CircleAlert, RotateCcw } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	const isYouTube = $derived(playerStore.nowPlaying?.sourceType === 'youtube');
	const locked = $derived(isYouTube || !eqStore.enabled);

	const TRACK_HEIGHT = 260;
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

	function fill(gain: number): { top: string; height: string } {
		const zero = gainToY(0);
		const at = gainToY(gain);
		return gain >= 0
			? { top: `${at}px`, height: `${zero - at}px` }
			: { top: `${zero}px`, height: `${at - zero}px` };
	}

	function formatGain(gain: number): string {
		return `${gain > 0 ? '+' : ''}${gain.toFixed(gain % 1 === 0 ? 0 : 1)}`;
	}

	function handlePointerDown(index: number, e: PointerEvent): void {
		if (locked) return;
		e.preventDefault();
		draggingIndex = index;
		(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
		updateGainFromPointer(index, e);
	}

	function handlePointerMove(index: number, e: PointerEvent): void {
		if (draggingIndex === index) updateGainFromPointer(index, e);
	}

	function updateGainFromPointer(index: number, e: PointerEvent): void {
		const track = trackRefs[index];
		if (!track) return;
		eqStore.setBandGain(index, yToGain(e.clientY - track.getBoundingClientRect().top));
	}

	function handlePresetChange(e: Event): void {
		const value = (e.target as HTMLSelectElement).value;
		if (value) eqStore.applyPreset(value as EqPresetName);
	}

	// Arrow keys step a band by half a decibel, Home and End jump to the limits.
	function handleBandKeydown(index: number, e: KeyboardEvent): void {
		if (locked) return;
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
</script>

<div class="flex flex-col gap-4 px-5 py-4">
	<div class="flex items-center gap-3">
		<input
			type="checkbox"
			class="toggle toggle-primary"
			checked={eqStore.enabled}
			disabled={isYouTube}
			onchange={() => eqStore.toggleEq()}
			aria-label="Toggle equalizer"
		/>
		<span class="dn-eq-state" aria-hidden="true">{eqStore.enabled ? 'On' : 'Off'}</span>
	</div>

	{#if isYouTube}
		<p
			class="flex items-center gap-2 rounded-field border border-warning/50 bg-warning/12 px-3 py-2 text-xs font-semibold text-warning"
		>
			<CircleAlert class="size-4 shrink-0" aria-hidden="true" />
			EQ is not available during YouTube playback.
		</p>
	{/if}

	<div class="flex items-center gap-2" class:dn-eq-locked={locked}>
		<span class="shrink-0 text-sm font-semibold" aria-hidden="true">Equalizer preset</span>
		<select
			class="select select-sm min-w-0 flex-1"
			aria-label="Equalizer preset"
			value={eqStore.activePreset ?? ''}
			onchange={handlePresetChange}
			disabled={locked}
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
			disabled={locked}
			onclick={() => eqStore.resetToFlat()}
		/>
	</div>

	<div class="dn-eq-bands" class:dn-eq-locked={locked}>
		<div class="dn-eq-scale" style:height="{TRACK_HEIGHT}px" aria-hidden="true">
			{#each DB_TICKS as tick (tick)}
				<span>{tick > 0 ? '+' : ''}{tick}</span>
			{/each}
		</div>

		{#each { length: EQ_BAND_COUNT } as _, i (i)}
			{@const band = fill(eqStore.gains[i])}
			<div class="dn-eq-band">
				<span class="dn-eq-value" aria-hidden="true">{formatGain(eqStore.gains[i])}</span>
				<div
					class="dn-eq-track"
					class:dn-eq-track--dragging={draggingIndex === i}
					style:height="{TRACK_HEIGHT}px"
					bind:this={trackRefs[i]}
					onpointerdown={(e) => handlePointerDown(i, e)}
					onpointermove={(e) => handlePointerMove(i, e)}
					onpointerup={() => (draggingIndex = null)}
					onpointercancel={() => (draggingIndex = null)}
					onkeydown={(e) => handleBandKeydown(i, e)}
					role="slider"
					tabindex={locked ? -1 : 0}
					aria-label="{EQ_FREQUENCY_LABELS[i]} Hz"
					aria-orientation="vertical"
					aria-disabled={locked}
					aria-valuemin={EQ_MIN_GAIN}
					aria-valuemax={EQ_MAX_GAIN}
					aria-valuenow={eqStore.gains[i]}
					aria-valuetext="{eqStore.gains[i]} dB"
				>
					<span class="dn-eq-rail"></span>
					<span class="dn-eq-zero" style:top="{gainToY(0)}px"></span>
					<span class="dn-eq-fill" style:top={band.top} style:height={band.height}></span>
					<span class="dn-eq-thumb" style:top="{gainToY(eqStore.gains[i]) - 8}px"></span>
				</div>
				<span class="dn-eq-hz" aria-hidden="true">{EQ_FREQUENCY_LABELS[i]}</span>
			</div>
		{/each}
	</div>
</div>

<style>
	.dn-eq-state {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.18em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-eq-locked {
		opacity: 0.45;
		pointer-events: none;
	}
	.dn-eq-bands {
		display: flex;
		gap: 0;
		padding-top: 0.25rem;
		transition: opacity 200ms;
	}
	.dn-eq-scale {
		display: flex;
		flex-direction: column;
		justify-content: space-between;
		width: 2rem;
		flex-shrink: 0;
		margin-top: 1.375rem;
		padding-right: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		line-height: 0;
		font-variant-numeric: tabular-nums;
		text-align: right;
		color: var(--dn-muted);
		user-select: none;
	}
	.dn-eq-band {
		display: flex;
		min-width: 1.75rem;
		flex: 1;
		flex-direction: column;
		align-items: center;
		gap: 0.5rem;
		user-select: none;
	}
	.dn-eq-value,
	.dn-eq-hz {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-variant-numeric: tabular-nums;
		line-height: 0.875rem;
	}
	.dn-eq-value {
		font-weight: 700;
	}
	.dn-eq-hz {
		color: var(--dn-muted);
	}
	.dn-eq-track {
		position: relative;
		width: 100%;
		cursor: pointer;
		touch-action: none;
		border-radius: 0.375rem;
	}
	.dn-eq-track:focus-visible {
		outline-offset: 0;
	}
	.dn-eq-rail,
	.dn-eq-fill,
	.dn-eq-thumb {
		position: absolute;
		left: 50%;
		translate: -50% 0;
	}
	.dn-eq-rail {
		top: 0;
		bottom: 0;
		width: 2px;
		background: var(--dn-line);
	}
	.dn-eq-zero {
		position: absolute;
		left: 18%;
		right: 18%;
		height: 1px;
		background: var(--dn-edge);
	}
	.dn-eq-fill {
		width: 4px;
		border-radius: 2px;
		background: var(--dn-title);
		transition:
			top 75ms,
			height 75ms;
	}
	.dn-eq-thumb {
		width: 16px;
		height: 16px;
		border: 2px solid var(--dn-action-edge);
		border-radius: 999px;
		background: var(--dn-action);
		box-shadow: 0 0 0 3px var(--color-base-200);
		transition:
			top 75ms,
			scale 75ms;
	}
	.dn-eq-track:hover .dn-eq-thumb {
		scale: 1.1;
	}
	.dn-eq-track--dragging .dn-eq-thumb {
		scale: 1.25;
	}
	@media (forced-colors: active) {
		.dn-eq-rail,
		.dn-eq-zero,
		.dn-eq-fill,
		.dn-eq-thumb {
			background: CanvasText;
			forced-color-adjust: none;
		}
	}
</style>
