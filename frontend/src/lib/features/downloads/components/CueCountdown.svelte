<script lang="ts">
	// The Wanted signature: a record sitting under a lifted needle. The amber ring fills
	// as the backoff wait elapses and a "needle head" dot creeps toward the cue point - when
	// it completes, the needle drops and the album starts pressing again. Still + amber here
	// deliberately contrasts the spinning + primary VinylProgress of a live download.
	interface Props {
		/** 0..1 - how much of the current wait has elapsed (drives the ring + needle head) */
		fraction?: number;
		/** center countdown, e.g. "11:58" or "7h 32m" */
		label?: string;
		size?: number;
		/** under a minute to go - draw attention to the imminent drop */
		urgent?: boolean;
	}
	let { fraction = 0, label = '', size = 72, urgent = false }: Props = $props();
	const pct = $derived(Math.round(Math.max(0, Math.min(1, fraction)) * 100));
</script>

<div
	class="cue"
	style="--cue-size:{size}px; --cue-pct:{pct};"
	role="img"
	aria-label={label ? `Next attempt in ${label}` : 'Waiting to retry'}
>
	<div class="cue-ring" aria-hidden="true"></div>
	<div class="cue-disc" aria-hidden="true"></div>
	<div class="cue-arm" class:is-urgent={urgent} aria-hidden="true">
		<span class="cue-head"></span>
	</div>
	<div class="cue-label">
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
			oklch(from var(--color-warning) l c h / 0.9) calc(var(--cue-pct) * 1%),
			oklch(from var(--color-base-content) l c h / 0.1) 0
		);
		-webkit-mask: radial-gradient(circle, transparent 58%, #000 60%);
		mask: radial-gradient(circle, transparent 58%, #000 60%);
		transition: background 0.9s linear;
	}
	.cue-disc {
		position: absolute;
		inset: 9%;
		border-radius: 9999px;
		opacity: 0.7;
		background:
			repeating-radial-gradient(
				circle at center,
				oklch(from var(--color-base-content) l c h / 0.06) 0px,
				oklch(from var(--color-base-content) l c h / 0.06) 1px,
				transparent 2px,
				transparent 4px
			),
			radial-gradient(
				circle at 34% 28%,
				oklch(from var(--color-base-content) l c h / 0.1),
				transparent 56%
			),
			radial-gradient(
				circle at center,
				oklch(from var(--color-base-300) l c h),
				oklch(from var(--color-base-100) l c h) 95%
			);
		box-shadow:
			inset 0 0 0 1px oklch(from var(--color-base-content) l c h / 0.08),
			inset 0 0 12px oklch(from var(--color-base-100) l c h / 0.9);
	}
	/* a dot pinned to 12 o'clock; rotating the wrapper to the fill angle walks it round the ring */
	.cue-arm {
		position: absolute;
		inset: 0;
		transform: rotate(calc(var(--cue-pct) * 3.6deg));
		transition: transform 0.9s linear;
	}
	.cue-head {
		position: absolute;
		top: -1px;
		left: 50%;
		width: 7px;
		height: 7px;
		margin-left: -3.5px;
		border-radius: 9999px;
		background: oklch(from var(--color-warning) l c h);
		box-shadow: 0 0 6px oklch(from var(--color-warning) l c h / 0.8);
	}
	.cue-arm.is-urgent .cue-head {
		/* scoped amber pulse - the global glow-pulse keyframes hardcode --color-primary and
		   would turn the amber needle head blue */
		animation: cue-head-pulse 1.2s ease-in-out infinite;
	}
	@keyframes cue-head-pulse {
		0%,
		100% {
			opacity: 1;
			box-shadow: 0 0 6px oklch(from var(--color-warning) l c h / 0.8);
		}
		50% {
			opacity: 0.6;
			box-shadow: 0 0 11px oklch(from var(--color-warning) l c h / 1);
		}
	}
	.cue-label {
		position: absolute;
		inset: 0;
		margin: auto;
		width: 60%;
		height: 60%;
		border-radius: 9999px;
		display: flex;
		align-items: center;
		justify-content: center;
		text-align: center;
		background: radial-gradient(
			circle at center,
			oklch(from var(--color-warning) l c h / 0.16),
			oklch(from var(--color-base-200) l c h)
		);
		box-shadow: inset 0 0 0 1px oklch(from var(--color-warning) l c h / 0.25);
	}
	.cue-time {
		font-size: calc(var(--cue-size) * 0.2);
		font-weight: 700;
		font-variant-numeric: tabular-nums;
		line-height: 1;
		color: oklch(from var(--color-warning) l c h);
	}
	@media (prefers-reduced-motion: reduce) {
		.cue-ring,
		.cue-arm {
			transition: none;
		}
		.cue-arm.is-urgent .cue-head {
			animation: none;
		}
	}
</style>
