<script lang="ts">
	import { QUALITY_TIERS as TIERS, tierIndex as idxOf } from './qualityTiers';

	// accepted band is min..max; scorer prefers the highest tier absolutely.
	// codec-agnostic lossy bands (MP3/AAC/OGG/Opus map in by bitrate)
	interface Props {
		minKey?: string;
		maxKey?: string;
	}
	let { minKey = $bindable('mp3_320'), maxKey = $bindable('lossless') }: Props = $props();

	const LAST = TIERS.length - 1;

	const minIdx = $derived(idxOf(minKey));
	const maxIdx = $derived(idxOf(maxKey));

	let railEl = $state<HTMLDivElement | null>(null);
	let dragging = $state<'min' | 'max' | null>(null);

	function setIdx(which: 'min' | 'max', idx: number) {
		const clamped = Math.max(0, Math.min(LAST, idx));
		if (which === 'min') minKey = TIERS[Math.min(clamped, maxIdx)].key;
		else maxKey = TIERS[Math.max(clamped, minIdx)].key;
	}

	function idxFromClientX(clientX: number): number {
		if (!railEl) return 0;
		const rect = railEl.getBoundingClientRect();
		const frac = rect.width ? (clientX - rect.left) / rect.width : 0;
		return Math.round(frac * LAST);
	}

	function startDrag(which: 'min' | 'max', e: PointerEvent) {
		dragging = which;
		(e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
	}
	function onMove(e: PointerEvent) {
		if (dragging) setIdx(dragging, idxFromClientX(e.clientX));
	}
	function endDrag() {
		dragging = null;
	}
	function onKey(which: 'min' | 'max', e: KeyboardEvent) {
		const cur = which === 'min' ? minIdx : maxIdx;
		let next = cur;
		if (e.key === 'ArrowRight' || e.key === 'ArrowUp') next = cur + 1;
		else if (e.key === 'ArrowLeft' || e.key === 'ArrowDown') next = cur - 1;
		else if (e.key === 'Home') next = 0;
		else if (e.key === 'End') next = LAST;
		else return;
		e.preventDefault();
		setIdx(which, next);
	}
	function nearestHandle(idx: number): 'min' | 'max' {
		return Math.abs(idx - minIdx) <= Math.abs(idx - maxIdx) ? 'min' : 'max';
	}

	const pct = (i: number) => `${(i / LAST) * 100}%`;
	const summary = $derived(
		minIdx === maxIdx
			? `${TIERS[minIdx].full} only · prefer the best available`
			: `Accept ${TIERS[minIdx].full} → ${TIERS[maxIdx].full} · always take the best available`
	);
</script>

<div class="qr">
	<div class="qr-pad">
		<div class="qr-rail" bind:this={railEl}>
			<div class="qr-fill" style="left:{pct(minIdx)}; right:calc(100% - {pct(maxIdx)})"></div>
			<button
				type="button"
				class="qr-thumb"
				style="left:{pct(minIdx)}"
				role="slider"
				aria-label="Minimum quality"
				aria-valuemin={0}
				aria-valuemax={LAST}
				aria-valuenow={minIdx}
				aria-valuetext={TIERS[minIdx].full}
				onpointerdown={(e) => startDrag('min', e)}
				onpointermove={onMove}
				onpointerup={endDrag}
				onpointercancel={endDrag}
				onkeydown={(e) => onKey('min', e)}
			></button>
			<button
				type="button"
				class="qr-thumb"
				style="left:{pct(maxIdx)}"
				role="slider"
				aria-label="Maximum quality"
				aria-valuemin={0}
				aria-valuemax={LAST}
				aria-valuenow={maxIdx}
				aria-valuetext={TIERS[maxIdx].full}
				onpointerdown={(e) => startDrag('max', e)}
				onpointermove={onMove}
				onpointerup={endDrag}
				onpointercancel={endDrag}
				onkeydown={(e) => onKey('max', e)}
			></button>
		</div>
	</div>
	<div class="qr-labels">
		{#each TIERS as t, i (t.key)}
			<button
				type="button"
				class="qr-label"
				class:in-range={i >= minIdx && i <= maxIdx}
				onclick={() => setIdx(nearestHandle(i), i)}
			>
				{t.label}
			</button>
		{/each}
	</div>
	<p class="qr-summary">{summary}</p>
</div>

<style>
	.qr {
		--thumb: 18px;
		width: 100%;
	}
	/* pad by the thumb radius so the end thumbs sit fully on-screen */
	.qr-pad {
		padding: 0 calc(var(--thumb) / 2);
	}
	.qr-rail {
		position: relative;
		height: var(--thumb);
		touch-action: none;
	}
	.qr-rail::before {
		content: '';
		position: absolute;
		inset-inline: 0;
		top: 50%;
		height: 6px;
		transform: translateY(-50%);
		border-radius: 9999px;
		background: oklch(from var(--color-base-300) l c h);
	}
	.qr-fill {
		position: absolute;
		top: 50%;
		height: 6px;
		transform: translateY(-50%);
		border-radius: 9999px;
		background: linear-gradient(
			90deg,
			oklch(from var(--color-primary) l c h / 0.7),
			oklch(from var(--color-primary) l c h)
		);
	}
	.qr-thumb {
		position: absolute;
		top: 50%;
		width: var(--thumb);
		height: var(--thumb);
		transform: translate(-50%, -50%);
		border-radius: 9999px;
		background: oklch(from var(--color-primary) l c h);
		box-shadow: 0 0 0 4px oklch(from var(--color-primary) l c h / 0.18);
		cursor: grab;
		transition: box-shadow 0.15s ease;
	}
	.qr-thumb:active {
		cursor: grabbing;
	}
	.qr-thumb:focus-visible {
		outline: none;
		box-shadow: 0 0 0 4px oklch(from var(--color-primary) l c h / 0.45);
	}
	.qr-labels {
		display: flex;
		justify-content: space-between;
		margin-top: 0.4rem;
	}
	.qr-label {
		font-size: 0.7rem;
		font-weight: 600;
		color: oklch(from var(--color-base-content) l c h / 0.45);
		transition: color 0.15s ease;
	}
	.qr-label.in-range {
		color: oklch(from var(--color-primary) l c h);
	}
	.qr-label:hover {
		color: oklch(from var(--color-base-content) l c h / 0.8);
	}
	.qr-summary {
		margin-top: 0.6rem;
		font-size: 0.8rem;
		color: oklch(from var(--color-base-content) l c h / 0.6);
	}
</style>
