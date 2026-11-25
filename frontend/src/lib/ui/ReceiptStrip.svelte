<!--
	A strip of till paper with a cut right end: a row of cells, each a figure
	over a mono label ("459" / "PLAYS, AND YOUR SHELVES"), and a link at the
	end ("THE RECEIPT >"). A `struck` figure is crossed out (counted, then
	left out); a `total` prints large in Anton. A cell with no figure is a
	line of its own, for a strip that only says something. `print` runs it
	out across the rail in steps (0.9s), once, when it first shows. In a
	narrow place (a phone) the cells go two to a row, the link under them.
-->
<script lang="ts" module>
	export interface ReceiptCell {
		figure?: string;
		label: string;
		struck?: boolean;
		total?: boolean;
	}
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		cells: ReceiptCell[];
		/** The list's name for a screen reader. */
		label?: string;
		/** A link or button at the strip's end. */
		link?: Snippet;
		print?: boolean;
		/** Layout only: margin, width. */
		class?: string;
	}

	let { cells, label, link, print = false, class: className = '' }: Props = $props();
</script>

<div class="dn-rstrip {className}" class:is-printing={print}>
	<div class="dn-rstrip__paper">
		<ul class="dn-rstrip__cells" aria-label={label}>
			{#each cells as cell, i (i)}
				<li class="dn-rstrip__cell" class:is-struck={cell.struck} class:is-total={cell.total}>
					{#if cell.figure !== undefined}<b>{cell.figure}</b>{/if}
					<span class:is-alone={cell.figure === undefined}>{cell.label}</span>
				</li>
			{/each}
		</ul>
		{#if link}<div class="dn-rstrip__link">{@render link()}</div>{/if}
	</div>
</div>

<style>
	/* the drop shadow follows the cut end */
	.dn-rstrip {
		container-type: inline-size;
		rotate: -0.25deg;
		filter: drop-shadow(0 8px 8px rgb(0 0 0 / 0.4));
	}

	.dn-rstrip__paper {
		--dn-muted: color-mix(in oklab, var(--dn-paper-content) 74%, var(--dn-paper-white));
		--dn-title: var(--dn-paper-content);
		display: flex;
		align-items: center;
		min-height: 3.75rem;
		padding: 0.375rem 0.75rem 0.375rem 0.375rem;
		background-color: var(--dn-paper-white);
		color: var(--dn-paper-content);
		clip-path: polygon(
			0 0,
			100% 0,
			99.4% 12%,
			100% 25%,
			99.4% 37%,
			100% 50%,
			99.4% 62%,
			100% 75%,
			99.4% 88%,
			100% 100%,
			0 100%
		);
	}

	.dn-rstrip__cells {
		display: flex;
		flex: 1;
		min-width: 0;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.dn-rstrip__cell {
		position: relative;
		display: flex;
		flex: 1 1 0;
		flex-direction: column;
		justify-content: center;
		gap: 0.1875rem;
		min-width: 0;
		padding: 0 0.75rem 0 1rem;
		overflow-wrap: normal;
	}
	.dn-rstrip__cell + .dn-rstrip__cell::before {
		content: '';
		position: absolute;
		left: 0;
		top: -0.25rem;
		bottom: -0.25rem;
		border-left: 1.5px dashed color-mix(in oklab, var(--dn-paper-content) 30%, transparent);
	}

	.dn-rstrip__cell b {
		font-family: var(--font-mono);
		font-size: 1.1875rem;
		font-weight: 700;
		line-height: 1;
		letter-spacing: -0.01em;
	}
	.dn-rstrip__cell.is-struck b {
		text-decoration: line-through;
		text-decoration-thickness: 2px;
	}
	.dn-rstrip__cell.is-total b {
		font-family: var(--font-display);
		font-size: 1.875rem;
		font-weight: 400;
		line-height: 0.9;
		font-synthesis: none;
	}
	.dn-rstrip__cell span {
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 600;
		line-height: 1.15;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-rstrip__cell span.is-alone {
		font-size: 0.6875rem;
		text-align: center;
	}

	.dn-rstrip__link {
		flex: none;
	}

	@container (max-width: 30rem) {
		.dn-rstrip__paper {
			flex-wrap: wrap;
			padding: 0.625rem 0.75rem 0.25rem 0.375rem;
		}
		.dn-rstrip__cells {
			display: grid;
			flex-basis: 100%;
			grid-template-columns: repeat(2, minmax(0, 1fr));
			row-gap: 0.75rem;
		}
		.dn-rstrip__cell:nth-child(odd)::before {
			content: none;
		}
		.dn-rstrip__cell.is-total:nth-child(odd) {
			grid-column: 1 / -1;
		}
		.dn-rstrip__link {
			flex-basis: 100%;
			display: flex;
			justify-content: flex-end;
		}
	}

	/* printing: the strip comes out left to right; the margin keeps its shadow */
	@keyframes dn-rstrip-print {
		from {
			clip-path: inset(-2rem 100% -2rem -2rem);
		}
		to {
			clip-path: inset(-2rem);
		}
	}
	.dn-rstrip.is-printing {
		animation: dn-rstrip-print 900ms steps(18) both;
	}

	@media (prefers-contrast: more) {
		.dn-rstrip {
			filter: none;
		}
		.dn-rstrip__paper {
			clip-path: none;
			border: 1px solid currentColor;
		}
	}
</style>
