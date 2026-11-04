<!--
	The frit: graded dots at a hero's top corner or along a page's bottom edge.
	One decorative layer that fills its nearest positioned ancestor's edge, so
	the parent must be `relative` and keep text off that edge. Hidden from
	screen readers and pointers, gone under high contrast and forced colours,
	and still. The dots are a mask over the theme's frit colour.
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

	.dn-frit--top-right,
	.dn-frit--top-left {
		top: 0;
		width: min(30rem, 55%);
		height: 20rem;
		mask-size: 30rem 20rem;
		mask-position: top right;
		-webkit-mask-size: 30rem 20rem;
		-webkit-mask-position: top right;
	}
	.dn-frit--top-right {
		right: 0;
	}
	.dn-frit--top-left {
		left: 0;
		transform: scaleX(-1);
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

	@media (max-width: 479px) {
		.dn-frit--top-right,
		.dn-frit--top-left {
			height: 12rem;
			mask-size: 18rem 12rem;
			-webkit-mask-size: 18rem 12rem;
		}
	}
</style>
