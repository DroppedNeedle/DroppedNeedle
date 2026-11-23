<!--
	Where one thing sits in a ranked list, drawn as a strip of ticks with one
	inked: a pick's place among an artist's neighbours, "5th closest of 100".
	It is one image named by `label`, which is also printed beside the inked
	tick (towards the middle, so it never runs off the end). It draws in the
	text colour, so it reads on paper and on the page.
-->
<script lang="ts">
	interface Props {
		/** How many places the list has. */
		ticks?: number;
		/** The inked place, from 1. */
		at: number;
		/** The sentence: "5th closest of 100". */
		label: string;
		/** Layout only: width, margin. */
		class?: string;
	}

	let { ticks = 100, at, label, class: className = '' }: Props = $props();

	const count = $derived(Math.max(1, Math.round(ticks)));
	const place = $derived(Math.min(count, Math.max(1, Math.round(at))));
	const step = $derived(`${100 / count}%`);
	const mark = $derived(`${((place - 0.5) / count) * 100}%`);
	const towardsEnd = $derived(place / count > 0.6);
</script>

<div
	class="dn-rank {className}"
	role="img"
	aria-label={label}
	style:--rank-step={step}
	style:--rank-at={mark}
>
	<span class="dn-rank__strip" aria-hidden="true"><span class="dn-rank__mark"></span></span>
	<span class="dn-rank__caption" aria-hidden="true">
		<span class="dn-rank__label" class:is-end={towardsEnd}>{label}</span>
	</span>
</div>

<style>
	.dn-rank {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.dn-rank__strip {
		position: relative;
		height: 0.875rem;
		border-left: 1.5px solid currentColor;
		background-image: repeating-linear-gradient(
			to right,
			color-mix(in oklab, currentColor 34%, transparent) 0 1px,
			transparent 1px var(--rank-step)
		);
	}

	.dn-rank__mark {
		position: absolute;
		top: -0.25rem;
		bottom: -0.25rem;
		left: var(--rank-at);
		width: 4px;
		margin-left: -2px;
		border-radius: 1px;
		background-color: currentColor;
	}

	.dn-rank__caption {
		position: relative;
		height: 0.625rem;
	}
	.dn-rank__label {
		position: absolute;
		top: 0;
		left: var(--rank-at);
		translate: -2px 0;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		line-height: 1;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		white-space: nowrap;
	}
	.dn-rank__label.is-end {
		left: auto;
		right: calc(100% - var(--rank-at));
		translate: 2px 0;
	}

	@media (forced-colors: active) {
		.dn-rank__strip {
			background-image: none;
			border-bottom: 1px solid CanvasText;
		}
		.dn-rank__mark {
			background-color: CanvasText;
		}
	}
</style>
