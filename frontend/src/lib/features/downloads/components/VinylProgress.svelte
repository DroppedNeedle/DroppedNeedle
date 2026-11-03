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
	style="--vinyl-size:{size}px; --vinyl-pct:{clamped};"
	role="img"
	aria-label={indeterminate ? 'Searching' : `${clamped}% downloaded`}
>
	{#if spinning}
		<div class="vinyl-halo" aria-hidden="true"></div>
	{/if}
	<div class="vinyl-ring" aria-hidden="true"></div>
	<div class="vinyl-disc" class:is-spinning={spinning} aria-hidden="true"></div>
	<div class="vinyl-label">
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
	.vinyl-halo {
		position: absolute;
		inset: -6px;
		border-radius: 9999px;
		background: radial-gradient(
			circle,
			oklch(from var(--color-primary) l c h / 0.32),
			transparent 70%
		);
		animation: glow-pulse 2.5s ease-in-out infinite;
		pointer-events: none;
	}
	.vinyl-ring {
		position: absolute;
		inset: 0;
		border-radius: 9999px;
		background: conic-gradient(
			oklch(from var(--color-primary) l c h / 0.95) calc(var(--vinyl-pct) * 1%),
			oklch(from var(--color-base-content) l c h / 0.1) 0
		);
		-webkit-mask: radial-gradient(circle, transparent 58%, #000 60%);
		mask: radial-gradient(circle, transparent 58%, #000 60%);
		transition: background 0.6s var(--ease-spring, ease-out);
	}
	.vinyl-disc {
		position: absolute;
		inset: 9%;
		border-radius: 9999px;
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
	.vinyl-disc.is-spinning {
		animation: spin-vinyl 4.2s linear infinite;
	}
	.vinyl-label {
		position: absolute;
		inset: 0;
		margin: auto;
		width: 44%;
		height: 44%;
		border-radius: 9999px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: radial-gradient(
			circle at center,
			oklch(from var(--color-primary) l c h / 0.22),
			oklch(from var(--color-base-200) l c h)
		);
		box-shadow: inset 0 0 0 1px oklch(from var(--color-primary) l c h / 0.28);
	}
	.vinyl-pct {
		font-size: calc(var(--vinyl-size) * 0.2);
		font-weight: 700;
		font-variant-numeric: tabular-nums;
		line-height: 1;
		color: oklch(from var(--color-primary) l c h);
	}
	.vinyl-sym {
		font-size: 0.6em;
		opacity: 0.7;
	}
	.vinyl-dots {
		font-size: calc(var(--vinyl-size) * 0.22);
		letter-spacing: 0.05em;
		color: oklch(from var(--color-primary) l c h);
		animation: glow-pulse 1.6s ease-in-out infinite;
	}
	@media (prefers-reduced-motion: reduce) {
		.vinyl-disc.is-spinning,
		.vinyl-halo,
		.vinyl-dots {
			animation: none;
		}
		.vinyl-ring {
			transition: none;
		}
	}
</style>
