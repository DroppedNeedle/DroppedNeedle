<!--
	The wait before a still-hunting album's next try: an amber ring that
	fills as the wait passes, round "Next spin in" and the time left.
-->
<script lang="ts">
	interface Props {
		/** 0..1 - how much of the current wait has elapsed */
		fraction?: number;
		/** the time left, e.g. "11:58" or "7h 32m" */
		label?: string;
		size?: number;
		/** under a minute to go */
		urgent?: boolean;
	}
	let { fraction = 0, label = '', size = 136, urgent = false }: Props = $props();
	const pct = $derived(Math.round(Math.max(0, Math.min(1, fraction)) * 100));
</script>

<div
	class="cue"
	class:is-urgent={urgent}
	style:--cue-size="{size}px"
	style:--cue-pct={pct}
	role="img"
	aria-label={label ? `Next attempt in ${label}` : 'Waiting to retry'}
>
	<div class="cue-ring" aria-hidden="true"></div>
	<div class="cue-face" aria-hidden="true">
		<span class="cue-caption">Next spin in</span>
		<span class="cue-time">{label}</span>
	</div>
</div>

<style>
	.cue {
		position: relative;
		width: var(--cue-size);
		height: var(--cue-size);
		flex-shrink: 0;
	}
	.cue-ring {
		position: absolute;
		inset: 0;
		border-radius: 9999px;
		background: conic-gradient(
			var(--color-warning) calc(var(--cue-pct) * 1%),
			color-mix(in oklab, var(--color-warning) 22%, transparent) 0
		);
		-webkit-mask: radial-gradient(circle closest-side, transparent 88%, #000 90%);
		mask: radial-gradient(circle closest-side, transparent 88%, #000 90%);
		transition: background 0.9s linear;
	}
	.cue-face {
		position: absolute;
		inset: 13%;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.25rem;
		border-radius: 9999px;
		text-align: center;
	}
	.cue-caption {
		font-family: var(--font-mono);
		font-size: calc(var(--cue-size) * 0.065);
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.cue-time {
		font-family: var(--font-display);
		font-size: calc(var(--cue-size) * 0.2);
		font-weight: 400;
		font-synthesis: none;
		line-height: 1;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
		color: var(--color-base-content);
	}
	.is-urgent .cue-time {
		color: var(--color-warning);
	}
	@media (prefers-reduced-motion: reduce) {
		.cue-ring {
			transition: none;
		}
	}
</style>
