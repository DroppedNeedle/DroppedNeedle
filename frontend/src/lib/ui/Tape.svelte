<!--
	A strip of tape stuck on paper, its ends torn, carrying a short caps word:
	"NEW TO YOU" on a pick card. `acid` is the main label, `ink` a second one
	beside it ("FURTHER OUT", "EXTRA", or both on one tape, so a card never
	holds three), `warn` an amber notice that always carries its warning
	sign (heard as "Warning"), and `embossed` a raised black label with white letters and a small
	trace, the mark of something found by sound. `sm` is a small label for a
	row or a player bar. The tilt goes under reduced motion and the torn ends
	under high contrast.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { TriangleAlert } from 'lucide-svelte';

	interface Props {
		look?: 'acid' | 'ink' | 'warn' | 'embossed';
		size?: 'md' | 'sm';
		/** Layout only: margin, flex. */
		class?: string;
		children: Snippet;
	}

	let { look = 'acid', size = 'md', class: className = '', children }: Props = $props();
</script>

<span class="dn-tape {className}" data-look={look} data-size={size}>
	{#if look === 'warn'}
		<TriangleAlert class="dn-tape__icon" role="img" aria-label="Warning" />
	{:else if look === 'embossed'}
		<svg class="dn-tape__trace dn-decor" viewBox="0 0 26 12" aria-hidden="true">
			<path d="M1 6h4l2-4 3 8 3-7 2 5 2-3h8" />
		</svg>
	{/if}
	{@render children()}
</span>

<style>
	.dn-tape {
		display: inline-block;
		padding: 0.4rem 1.1rem;
		background-color: color-mix(in oklab, var(--dn-action) 88%, transparent);
		color: var(--dn-action-content);
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.18em;
		text-transform: uppercase;
		rotate: -2.5deg;
		box-shadow: 0 1px 3px rgb(0 0 0 / 0.18);
		clip-path: polygon(
			0 6%,
			3% 0,
			97% 4%,
			100% 0,
			99% 50%,
			100% 100%,
			96% 94%,
			2% 100%,
			0 92%,
			1.5% 50%
		);
	}

	.dn-tape[data-look='ink'] {
		background-color: var(--dn-paper-content);
		color: var(--dn-paper);
		rotate: 3deg;
	}

	.dn-tape[data-look='warn'],
	.dn-tape[data-look='embossed'] {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		white-space: nowrap;
	}

	.dn-tape[data-look='warn'] {
		background-color: var(--color-warning);
		color: var(--color-warning-content);
		rotate: -0.6deg;
	}
	.dn-tape :global(.dn-tape__icon) {
		width: 0.9375rem;
		height: 0.9375rem;
		flex: none;
	}

	/* a label-maker strip: black, white raised letters, no torn ends */
	.dn-tape[data-look='embossed'] {
		padding: 0.34rem 0.8rem 0.34rem 0.6rem;
		border-radius: 2px;
		background: linear-gradient(
			color-mix(in oklab, var(--dn-vinyl), white 7%),
			var(--dn-vinyl) 55%,
			color-mix(in oklab, var(--dn-vinyl), white 5%)
		);
		color: var(--dn-arm);
		letter-spacing: 0.22em;
		text-shadow:
			0 1px 0 rgb(0 0 0 / 0.9),
			0 -1px 0 rgb(255 255 255 / 0.25);
		box-shadow:
			0 1px 2px rgb(0 0 0 / 0.5),
			inset 0 1px 0 rgb(255 255 255 / 0.14),
			inset 0 -1px 0 rgb(0 0 0 / 0.6);
		clip-path: none;
		rotate: 1.5deg;
	}
	.dn-tape__trace {
		width: 1.625rem;
		height: 0.75rem;
		flex: none;
		fill: none;
		stroke: var(--dn-sound);
		stroke-width: 1.6;
		stroke-linecap: round;
		stroke-linejoin: round;
	}

	.dn-tape[data-size='sm'] {
		padding: 0.125rem 0.5rem 0.125rem 0.375rem;
		font-size: 0.5625rem;
		letter-spacing: 0.16em;
		gap: 0.3rem;
	}
	.dn-tape[data-size='sm'] .dn-tape__trace {
		width: 1.125rem;
		height: 0.5rem;
	}

	@media (max-width: 639px) {
		.dn-tape:not([data-size='sm']) {
			padding: 0.3rem 0.75rem;
			font-size: 0.6875rem;
			letter-spacing: 0.12em;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.dn-tape {
			rotate: none;
		}
	}

	@media (prefers-contrast: more) {
		.dn-tape {
			clip-path: none;
			box-shadow: none;
			border: 1px solid currentColor;
		}
	}
</style>
