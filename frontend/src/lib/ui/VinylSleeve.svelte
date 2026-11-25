<!--
	A cover printed on a sleeve, with a record half out of it behind. The
	sleeve is a thin paper border round the cover with a lifted shadow and a
	slight tilt; the cover inside, passed as children, is never filtered,
	cropped or covered. The record is glossy black with grooves, two sheens
	where light catches it and an acid label. It is decoration only (hidden
	from screen readers, still, gone under high contrast). `class` sizes the
	whole piece: the sleeve takes 72% of its width and the record the rest.
	On a phone the record is smaller, and under 480px it goes so the cover
	keeps the room. `playing` slides the record a tenth of the width further
	out (leave that room beside the piece), so its label
	(`label`, printed on it) shows and turns at 33 a minute; under reduced
	motion it stays home and still.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		/** Layout only: the width of cover and record together. */
		class?: string;
		/** The record is out and turning: something on it is playing. */
		playing?: boolean;
		/** Short text printed on the record's label, e.g. "WK 47 / NO 3". */
		label?: string;
		children: Snippet;
	}

	let { class: className = '', playing = false, label, children }: Props = $props();
</script>

<div class="dn-sleeve {className}" class:is-playing={playing}>
	<div class="dn-decor dn-sleeve__record" aria-hidden="true">
		{#if label}<span class="dn-sleeve__mark">{label}</span>{/if}
	</div>
	<div class="dn-sleeve__cover">
		<div class="dn-sleeve__art">
			{@render children()}
		</div>
	</div>
</div>

<style>
	.dn-sleeve {
		position: relative;
		container-type: inline-size;
	}

	/* The printed sleeve: paper stock round the art, lifted off the page. */
	.dn-sleeve__cover {
		position: relative;
		z-index: 1;
		width: 72cqw;
		padding: clamp(4px, 2cqw, 8px);
		border-radius: 5px;
		background-color: var(--dn-paper);
		box-shadow:
			0 1px 2px rgb(0 0 0 / 0.3),
			0 0 0 1px rgb(0 0 0 / 0.06),
			6px 14px 30px -10px rgb(0 0 0 / 0.55);
		rotate: -1.5deg;
	}

	.dn-sleeve__art {
		overflow: hidden;
		border-radius: 1px;
	}

	/* The record: a dark gloss under fine grooves, two sheens crossing it,
	   a faint bright rim, and the label with its ring and spindle hole. */
	.dn-sleeve__record {
		--label: var(--dn-action);
		--label-ring: color-mix(in oklab, var(--dn-action), #000 38%);
		--sheen: rgb(255 255 255 / 0.17);
		position: absolute;
		top: 50%;
		left: 30cqw;
		width: 68cqw;
		aspect-ratio: 1;
		translate: 0 -50%;
		border-radius: 9999px;
		pointer-events: none;
		background:
			radial-gradient(circle, var(--dn-vinyl) 0 1.6%, transparent 2%),
			radial-gradient(
				circle,
				var(--label) 0 27.4%,
				var(--label-ring) 27.8% 29.2%,
				var(--label) 29.6% 30.8%,
				var(--dn-vinyl) 31.2% 33%,
				transparent 33.4%
			),
			radial-gradient(ellipse 28% 16% at 74% 20%, rgb(255 255 255 / 0.09), transparent 100%),
			conic-gradient(
				transparent 0deg,
				var(--sheen) 34deg,
				transparent 64deg,
				transparent 112deg,
				var(--sheen) 138deg,
				transparent 164deg,
				transparent 200deg,
				rgb(255 255 255 / 0.08) 214deg,
				transparent 244deg,
				transparent 360deg
			),
			repeating-radial-gradient(
				circle,
				transparent 0,
				rgb(255 255 255 / 0.03) 0.3cqw,
				transparent 0.6cqw
			),
			radial-gradient(
				circle,
				var(--dn-vinyl) 0 34%,
				color-mix(in oklab, var(--dn-vinyl-groove) 30%, var(--dn-vinyl)) 70%,
				var(--dn-vinyl) 95%,
				var(--dn-vinyl-groove) 98.5%,
				var(--dn-vinyl) 100%
			);
		box-shadow:
			inset 0 0 0 1.5px rgb(255 255 255 / 0.16),
			0 12px 30px -10px rgb(0 0 0 / 0.6);
		transition: translate 700ms var(--ease-spring);
	}
	/* a tenth of the piece's width further out: the caller leaves that room beside it */
	.is-playing .dn-sleeve__record {
		translate: 10cqw -50%;
	}

	/* The label's print, in the label's middle; it turns with the record. */
	.dn-sleeve__mark {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		padding: 0 38%;
		font-family: var(--font-mono);
		font-size: max(7px, 2.1cqw);
		font-weight: 700;
		line-height: 1.15;
		letter-spacing: 0.06em;
		text-align: center;
		text-transform: uppercase;
		color: var(--dn-vinyl);
		rotate: -24deg;
	}
	.is-playing .dn-sleeve__mark {
		animation: dn-sleeve-turn 1.8s linear infinite;
	}
	@keyframes dn-sleeve-turn {
		from {
			rotate: -24deg;
		}
		to {
			rotate: 336deg;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.dn-sleeve__cover {
			rotate: none;
		}
		.is-playing .dn-sleeve__record {
			translate: 0 -50%;
		}
		.is-playing .dn-sleeve__mark {
			animation: none;
		}
	}

	@media (max-width: 767px) {
		.dn-sleeve__cover {
			width: 80cqw;
		}
		.dn-sleeve__record {
			left: 32cqw;
			width: 64cqw;
		}
	}

	@media (max-width: 479px) {
		.dn-sleeve__cover {
			width: 100cqw;
		}
		.dn-sleeve__record {
			display: none;
		}
	}
</style>
