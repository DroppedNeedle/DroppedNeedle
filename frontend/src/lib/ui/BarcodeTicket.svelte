<!--
	A tilted paper ticket. Given a `label` it is the design's signature: a
	barcode, a mono label under it ("DN / CC 3.2") and a block of diagonal
	stripes. The bars encode nothing, so that ticket is hidden from screen
	readers and gone under high contrast; `class` sets its width and
	everything inside scales with it.
	Given a `figure` it carries real facts instead: one big figure, a word
	beside it and a line over the stripes (the week ticket: "47", "WEEK",
	"SET OUT FRI 20 NOV"). That ticket is read as text, `spoken` when given,
	and only its stripes are decoration.
-->
<script lang="ts">
	import { withBasePath } from '$lib/utils/basePath';

	interface OrnamentProps {
		label: string;
		figure?: never;
		lines?: never;
		spoken?: never;
		/** Layout only: width, margin. */
		class?: string;
	}

	interface FactsProps {
		label?: never;
		/** The big figure. */
		figure: string;
		/** The word beside the figure and the line over the stripes. */
		lines: readonly [string, string];
		/** What a screen reader hears, when the printed words read badly aloud. */
		spoken?: string;
		/** Layout only: width, margin. */
		class?: string;
	}

	type Props = OrnamentProps | FactsProps;

	let { label = '', figure, lines, spoken, class: className = '' }: Props = $props();

	const bars = `url("${withBasePath('/img/barcode.svg')}")`;
	// a longer label prints smaller, so it always fits beside the stripes
	const labelSize = $derived(`${Math.min(8, 90 / Math.max(label.length, 1)).toFixed(2)}cqw`);
</script>

{#if figure !== undefined && lines}
	<p class="dn-ticket-facts {className}">
		{#if spoken}<span class="sr-only">{spoken}</span>{/if}
		<span class="dn-ticket-facts__no" aria-hidden={spoken ? 'true' : undefined}>
			<b>{figure}</b>
			<span class="dn-ticket-facts__unit">{lines[0]}</span>
		</span>
		<span class="dn-ticket-facts__day" aria-hidden={spoken ? 'true' : undefined}>
			<span>{lines[1]}</span>
			<span class="dn-ticket-facts__stripes dn-stripes dn-decor"></span>
		</span>
	</p>
{:else}
	<div class="dn-decor dn-ticket {className}" aria-hidden="true">
		<div class="dn-ticket__stock">
			<span class="dn-ticket__bars" style:mask-image={bars} style:-webkit-mask-image={bars}></span>
			<span class="dn-ticket__foot">
				<span class="dn-ticket__label" style:font-size={labelSize}>{label}</span>
				<span class="dn-ticket__stripes dn-stripes"></span>
			</span>
		</div>
	</div>
{/if}

<style>
	.dn-ticket {
		container-type: inline-size;
		pointer-events: none;
	}

	.dn-ticket__stock {
		display: flex;
		flex-direction: column;
		gap: 2.5cqw;
		padding: 4cqw 5cqw 4.5cqw;
		border-radius: 2px;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		box-shadow:
			0 0 0 1px color-mix(in oklab, var(--dn-paper-content) 12%, transparent),
			4px 10px 22px -10px rgb(0 0 0 / 0.7);
		rotate: -3deg;
	}

	.dn-ticket__bars {
		display: block;
		height: 18cqw;
		background-color: currentColor;
		mask-size: 100% 100%;
		mask-repeat: no-repeat;
		-webkit-mask-size: 100% 100%;
		-webkit-mask-repeat: no-repeat;
	}

	.dn-ticket__foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 3cqw;
	}

	.dn-ticket__label {
		font-family: var(--font-mono);
		min-width: 0;
		overflow: hidden;
		font-weight: 600;
		line-height: 1;
		letter-spacing: -0.01em;
		white-space: nowrap;
	}

	.dn-ticket__stripes {
		flex: 0 0 auto;
		width: 17cqw;
		height: 7.5cqw;
		font-size: 8cqw;
	}

	/* the ticket with facts on it: the same stock, torn at a dashed line */
	.dn-ticket-facts {
		display: inline-flex;
		align-items: stretch;
		height: 3.25rem;
		margin: 0;
		border-radius: 2px;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		box-shadow:
			0 0 0 1px color-mix(in oklab, var(--dn-paper-content) 12%, transparent),
			4px 10px 22px -10px rgb(0 0 0 / 0.7);
		rotate: -2.5deg;
		white-space: nowrap;
	}

	.dn-ticket-facts__no {
		display: flex;
		align-items: baseline;
		gap: 0.375rem;
		padding: 0.375rem 0.75rem 0;
		border-right: 1.5px dashed color-mix(in oklab, var(--dn-paper-content) 45%, transparent);
	}
	.dn-ticket-facts__no b {
		font-family: var(--font-display);
		font-size: 2.375rem;
		font-weight: 400;
		line-height: 1;
		font-synthesis: none;
	}
	.dn-ticket-facts__unit {
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}

	.dn-ticket-facts__day {
		display: flex;
		flex-direction: column;
		justify-content: center;
		gap: 0.3125rem;
		padding: 0 0.75rem;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 600;
		line-height: 1;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}
	.dn-ticket-facts__stripes {
		display: block;
		width: 4.625rem;
		height: 0.5rem;
		font-size: 0.6875rem;
	}
</style>
