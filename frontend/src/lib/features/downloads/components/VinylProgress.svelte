<!--
	A live download's record: a black disc whose acid outer ring is the
	progress, the percent in the middle. It turns slowly while `spinning`
	(the light on it moves; the numbers stay upright) and stands still under
	reduced motion. `indeterminate` (still searching) shows dots, no ring.
-->
<script lang="ts">
	interface Props {
		percent?: number;
		spinning?: boolean;
		size?: number;
		/** show a pulsing indeterminate label instead of a percentage */
		indeterminate?: boolean;
	}
	let { percent = 0, spinning = false, size = 76, indeterminate = false }: Props = $props();
	const clamped = $derived(Math.max(0, Math.min(100, Math.round(percent))));
</script>

<div
	class="vinyl"
	style:--vinyl-size="{size}px"
	style:--vinyl-pct={indeterminate ? 0 : clamped}
	role="img"
	aria-label={indeterminate ? 'Searching' : `${clamped}% downloaded`}
>
	<div class="vinyl-disc" aria-hidden="true"></div>
	<div class="vinyl-sheen" class:is-spinning={spinning} aria-hidden="true"></div>
	<div class="vinyl-ring" aria-hidden="true"></div>
	<div class="vinyl-label" aria-hidden="true">
		{#if indeterminate}
			<span class="vinyl-dots">•••</span>
		{:else}
			<span class="vinyl-pct">{clamped}<span class="vinyl-sym">%</span></span>
		{/if}
	</div>
</div>

<style>
	.vinyl {
		position: relative;
		width: var(--vinyl-size);
		height: var(--vinyl-size);
		flex-shrink: 0;
	}
	.vinyl-disc {
		position: absolute;
		inset: 0;
		border-radius: 9999px;
		background:
			repeating-radial-gradient(
				circle at center,
				var(--dn-vinyl-groove) 0 1px,
				transparent 1px 3.5px
			),
			var(--dn-vinyl);
		box-shadow: 0 6px 18px -10px rgb(0 0 0 / 0.7);
	}
	/* two soft wedges of light that turn with the record */
	.vinyl-sheen {
		position: absolute;
		inset: 0;
		border-radius: 9999px;
		background: conic-gradient(
			transparent 0deg,
			rgb(255 255 255 / 0.14) 30deg,
			transparent 70deg,
			transparent 180deg,
			rgb(255 255 255 / 0.1) 210deg,
			transparent 250deg
		);
	}
	.vinyl-sheen.is-spinning {
		animation: spin-vinyl 6s linear infinite;
	}
	/* the progress: an acid band round the rim, over a dim track */
	.vinyl-ring {
		position: absolute;
		inset: 3%;
		border-radius: 9999px;
		background: conic-gradient(
			var(--dn-action) calc(var(--vinyl-pct) * 1%),
			rgb(255 255 255 / 0.12) 0
		);
		-webkit-mask: radial-gradient(circle closest-side, transparent 83%, #000 85%);
		mask: radial-gradient(circle closest-side, transparent 83%, #000 85%);
		transition: background 0.6s ease-out;
	}
	.vinyl-label {
		position: absolute;
		inset: 0;
		margin: auto;
		width: 64%;
		height: 64%;
		border-radius: 9999px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--dn-vinyl);
		box-shadow: 0 0 0 1px rgb(255 255 255 / 0.06);
	}
	.vinyl-pct {
		font-family: var(--font-display);
		font-size: calc(var(--vinyl-size) * 0.26);
		font-weight: 400;
		font-synthesis: none;
		line-height: 1;
		font-variant-numeric: tabular-nums;
		color: var(--dn-action);
	}
	.vinyl-sym {
		font-size: 0.8em;
	}
	.vinyl-dots {
		font-size: calc(var(--vinyl-size) * 0.2);
		letter-spacing: 0.05em;
		color: var(--dn-action);
		animation: glow-pulse 1.6s ease-in-out infinite;
	}
	@media (prefers-reduced-motion: reduce) {
		.vinyl-ring {
			transition: none;
		}
	}
</style>
