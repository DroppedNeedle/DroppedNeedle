<!--
	The frit: graded dots at a hero's top corner or along a page's bottom edge.
	A corner runs down the side further than along the top, and never past
	its parent's bottom.
	One decorative layer that fills its nearest positioned ancestor's edge, so
	the parent must be `relative` and keep text off that edge. Hidden from
	screen readers and pointers, gone under high contrast and forced colours,
	and still. The dots are a mask over the theme's frit colour; a second,
	soft mask fades them out, so the field is densest at its corner or edge
	and thins to nothing with no edge of its own.
-->
<script lang="ts">
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		/** `bottom` runs along the bottom edge, heaviest at the two sides. */
		place?: 'top-right' | 'top-left' | 'bottom';
		/** Layout only: size and offset. */
		class?: string;
	}

	let { place = 'top-right', class: className = '' }: Props = $props();

	const file = $derived(place === 'bottom' ? '/img/frit-edge.svg' : '/img/frit-corner.svg');
</script>

<div
	class="dn-decor dn-frit dn-frit--{place} {className}"
	aria-hidden="true"
	style:--frit-url={`url("${withBasePath(file)}")`}
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

	/* The edge tile repeats across, its dots growing toward the bottom. Two
	   soft ellipses from the bottom corners keep only the outer ends, so the
	   dots thin out going up and toward the middle with no edge of their own. */
	.dn-frit--bottom {
		--frit-reach:
			radial-gradient(
				ellipse 40% 100% at 0% 100%,
				black 0%,
				rgb(0 0 0 / 0.55) 45%,
				transparent 100%
			),
			radial-gradient(
				ellipse 40% 100% at 100% 100%,
				black 0%,
				rgb(0 0 0 / 0.55) 45%,
				transparent 100%
			);
		right: 0;
		bottom: 0;
		left: 0;
		height: 100%;
		mask-image: var(--frit-url), var(--frit-reach);
		mask-size:
			25rem 10rem,
			100% 100%,
			100% 100%;
		mask-repeat: repeat-x, no-repeat, no-repeat;
		mask-position: bottom left;
		mask-composite: intersect, add, add;
		-webkit-mask-image: var(--frit-url), var(--frit-reach);
		-webkit-mask-size:
			25rem 10rem,
			100% 100%,
			100% 100%;
		-webkit-mask-repeat: repeat-x, no-repeat, no-repeat;
		-webkit-mask-position: bottom left;
		-webkit-mask-composite: source-in, source-over, source-over;
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
