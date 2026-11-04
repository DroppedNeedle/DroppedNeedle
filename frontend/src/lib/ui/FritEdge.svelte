<!--
	The frit at one band's own top corner, for a band that carries its own
	(the page's frame is FritFrame, mounted by the shell). A corner runs down
	the side further than along the top, and never past its parent's bottom.
	One decorative layer at its nearest positioned ancestor's corner, so the
	parent must be `relative` and keep text off that corner. Hidden from
	screen readers and pointers, gone under high contrast and forced colours,
	and still. The dots are a mask over the theme's frit colour; a second,
	soft mask fades them out, so the field is densest at its corner and thins
	to nothing with no edge of its own.
-->
<script lang="ts">
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		place?: 'top-right' | 'top-left';
		/** Layout only: size and offset. */
		class?: string;
	}

	let { place = 'top-right', class: className = '' }: Props = $props();
</script>

<div
	class="dn-decor dn-frit dn-frit--{place} {className}"
	aria-hidden="true"
	style:--frit-url={`url("${withBasePath('/img/frit-corner.svg')}")`}
></div>

<style>
	.dn-frit {
		position: absolute;
		pointer-events: none;
		background-color: var(--dn-frit);
		opacity: var(--dn-frit-opacity);
		mask-image: var(--frit-url);
		mask-repeat: no-repeat;
		-webkit-mask-image: var(--frit-url);
		-webkit-mask-repeat: no-repeat;
	}

	/* The corner tile's dots shrink away from its corner; the ellipse from
	   the same corner fades what is left, so the field runs out along the
	   top and down the side instead of stopping at the tile's edge. */
	.dn-frit--top-right,
	.dn-frit--top-left {
		--frit-w: 25rem;
		--frit-h: 37.5rem;
		--frit-reach: radial-gradient(
			ellipse 100% 100% at 100% 0%,
			black 0%,
			rgb(0 0 0 / 0.85) 35%,
			rgb(0 0 0 / 0.4) 65%,
			transparent 95%
		);
		top: 0;
		right: 0;
		width: min(var(--frit-w), 40%);
		/* Never taller than the parent, and the tile scales to the height it
		   gets, so its finest dots always end inside and nothing is cut off. */
		height: min(var(--frit-h), 100%);
		mask-image: var(--frit-url), var(--frit-reach);
		mask-size:
			auto 100%,
			100% 100%;
		mask-position: top right;
		mask-composite: intersect;
		-webkit-mask-image: var(--frit-url), var(--frit-reach);
		-webkit-mask-size:
			auto 100%,
			100% 100%;
		-webkit-mask-position: top right;
		-webkit-mask-composite: source-in;
	}
	.dn-frit--top-left {
		right: auto;
		left: 0;
		transform: scaleX(-1);
	}

	/* On paper the corners lean toward the acid, so the hero reads green-led. */
	:global([data-theme='light']) .dn-frit--top-right,
	:global([data-theme='light']) .dn-frit--top-left {
		background-color: color-mix(in oklab, var(--dn-frit) 55%, var(--dn-action));
		opacity: 0.8;
	}

	@media (max-width: 1023px) {
		.dn-frit--top-right,
		.dn-frit--top-left {
			--frit-w: 18rem;
			--frit-h: 27rem;
		}
	}

	@media (max-width: 479px) {
		.dn-frit--top-right,
		.dn-frit--top-left {
			--frit-w: 10rem;
			--frit-h: 15rem;
		}
	}
</style>
