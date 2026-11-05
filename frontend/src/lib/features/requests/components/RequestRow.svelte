<!--
	The frame every card on the Requests page shares: a bordered dark card
	with an optional tick, the square cover, the text, and the state and
	buttons on the right. `tone` edges a card that waits (amber) or failed
	(red). `href` lays one link over the whole card, under its controls.
	On a phone the right side drops below the text.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import Card from '$lib/ui/Card.svelte';
	import './requests.css';

	interface Props {
		tone?: 'warning' | 'error' | null;
		href?: string;
		linkLabel?: string;
		/** A round picture (an artist) in place of the square cover. */
		round?: boolean;
		lead?: Snippet;
		cover: Snippet;
		aside?: Snippet;
		below?: Snippet;
		children: Snippet;
	}

	let {
		tone = null,
		href,
		linkLabel,
		round = false,
		lead,
		cover,
		aside,
		below,
		children
	}: Props = $props();
</script>

<Card
	variant="bordered"
	padding="none"
	class="rq-row relative {href ? 'rq-row--link' : ''} {tone ? `rq-row--${tone}` : ''}"
>
	{#if href}
		<a {href} class="rq-row__link" aria-label={linkLabel}></a>
	{/if}
	<div class="rq-row__grid" class:rq-row__grid--lead={lead}>
		{#if lead}
			<div class="rq-row__lead">{@render lead()}</div>
		{/if}
		<div class="rq-row__cover" class:rq-row__cover--round={round}>{@render cover()}</div>
		<div class="rq-row__body">{@render children()}</div>
		{#if aside}
			<div class="rq-row__aside">{@render aside()}</div>
		{/if}
	</div>
	{#if below}
		<div class="rq-row__below">{@render below()}</div>
	{/if}
</Card>

<style>
	:global(.rq-row) {
		border-width: 1.5px;
		transition: border-color 150ms;
	}
	:global(.rq-row.rq-row--link:hover) {
		border-color: var(--dn-edge);
	}
	:global(.rq-row.rq-row--warning) {
		border-color: var(--color-warning);
	}
	:global(.rq-row.rq-row--error) {
		border-color: var(--color-error);
	}

	.rq-row__link {
		position: absolute;
		inset: 0;
		z-index: 0;
		border-radius: inherit;
	}
	.rq-row__link:focus-visible {
		outline-offset: -3px;
	}

	/* the text sits over the link, so only its own links and buttons catch clicks */
	.rq-row__grid {
		position: relative;
		z-index: 1;
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		grid-template-areas: 'cover body aside';
		align-items: center;
		gap: 0 1.5rem;
		padding: 1.25rem 1.5rem;
		pointer-events: none;
	}
	.rq-row__grid--lead {
		grid-template-columns: auto auto minmax(0, 1fr) auto;
		grid-template-areas: 'lead cover body aside';
	}
	.rq-row__grid :global(:is(a, button, input, select, label)) {
		pointer-events: auto;
	}

	.rq-row__lead {
		grid-area: lead;
		display: flex;
	}
	.rq-row__cover {
		grid-area: cover;
		width: 6.5rem;
		height: 6.5rem;
		overflow: hidden;
		border-radius: 0.375rem;
		background-color: var(--color-base-300);
	}
	.rq-row__cover--round {
		border-radius: 9999px;
	}
	.rq-row__body {
		grid-area: body;
		min-width: 0;
	}
	.rq-row__aside {
		grid-area: aside;
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.75rem;
	}

	.rq-row__below {
		position: relative;
		z-index: 1;
		padding: 0 1.5rem 1.5rem;
	}

	@media (max-width: 767px) {
		.rq-row__grid {
			grid-template-areas:
				'cover body'
				'aside aside';
			grid-template-columns: auto minmax(0, 1fr);
			gap: 0.875rem 0.875rem;
			padding: 0.875rem;
		}
		.rq-row__grid--lead {
			grid-template-columns: auto auto minmax(0, 1fr);
			grid-template-areas:
				'lead cover body'
				'aside aside aside';
		}
		.rq-row__cover {
			width: 4.5rem;
			height: 4.5rem;
		}
		.rq-row__aside {
			flex-direction: row;
			flex-wrap: wrap;
			align-items: center;
			justify-content: flex-end;
			gap: 0.5rem;
		}
		.rq-row__below {
			padding: 0 0.875rem 0.875rem;
		}
	}
	@media (max-width: 399px) {
		.rq-row__grid,
		.rq-row__grid--lead {
			gap: 0.75rem 0.625rem;
		}
		.rq-row__cover {
			width: 3.75rem;
			height: 3.75rem;
		}
	}
</style>
