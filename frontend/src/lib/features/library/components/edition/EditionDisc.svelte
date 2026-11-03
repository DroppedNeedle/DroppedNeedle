<!--
	The edition plate's disc, in the vinyl look of the download progress: its
	ring fills to the share of the edition's tracks the files hold. Accent
	when complete, warning with gaps or a best guess, faint with no edition,
	and primary, turning slowly, while the files are placed or written.
-->
<script lang="ts">
	import type { DiscTone } from './editionCopy';

	interface Props {
		/** 0 to 1. */
		fill: number;
		tone: DiscTone;
		spinning?: boolean;
		size?: number;
	}

	let { fill, tone, spinning = false, size = 40 }: Props = $props();

	const pct = $derived(Math.round(Math.max(0, Math.min(1, fill)) * 100));
</script>

<span
	class="disc tone-{tone}"
	class:spinning
	style="--disc-size:{size}px; --edition-disc-pct:{pct}"
	aria-hidden="true"
>
	<span class="ring"></span>
	<span class="grooves"></span>
	<span class="hole"></span>
</span>

<style>
	@property --edition-disc-pct {
		syntax: '<number>';
		inherits: true;
		initial-value: 0;
	}

	.disc {
		position: relative;
		display: inline-block;
		flex-shrink: 0;
		width: var(--disc-size);
		height: var(--disc-size);
		transition: --edition-disc-pct 300ms ease-out;
	}
	.tone-accent {
		--disc-colour: var(--color-accent);
	}
	.tone-warning {
		--disc-colour: var(--color-warning);
	}
	.tone-none {
		--disc-colour: oklch(from var(--color-base-content) l c h / 0.2);
	}
	.tone-busy {
		--disc-colour: var(--color-primary);
	}
	.ring {
		position: absolute;
		inset: 0;
		border-radius: 9999px;
		background: conic-gradient(
			var(--disc-colour) calc(var(--edition-disc-pct) * 1%),
			oklch(from var(--color-base-content) l c h / 0.12) 0
		);
		-webkit-mask: radial-gradient(circle, transparent 58%, black 61%);
		mask: radial-gradient(circle, transparent 58%, black 61%);
	}
	.tone-none .ring {
		background: var(--disc-colour);
	}
	.grooves {
		position: absolute;
		inset: 14%;
		border-radius: 9999px;
		background:
			repeating-radial-gradient(
				circle at center,
				oklch(from var(--color-base-content) l c h / 0.08) 0px,
				oklch(from var(--color-base-content) l c h / 0.08) 1px,
				transparent 2px,
				transparent 3px
			),
			radial-gradient(
				circle at 34% 28%,
				oklch(from var(--color-base-content) l c h / 0.14),
				oklch(from var(--color-base-content) l c h / 0.05) 60%
			);
	}
	.hole {
		position: absolute;
		inset: 40%;
		border-radius: 9999px;
		background: var(--disc-colour);
	}
	.spinning .ring,
	.spinning .grooves {
		animation: edition-disc-turn 6s linear infinite;
	}
	@keyframes edition-disc-turn {
		to {
			transform: rotate(1turn);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.disc {
			transition: none;
		}
		.spinning .ring,
		.spinning .grooves {
			animation: none;
		}
	}
</style>
