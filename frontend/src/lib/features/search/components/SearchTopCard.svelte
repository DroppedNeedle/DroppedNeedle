<!--
	A top result on torn paper: the picture, a mono kicker ("TOP ARTIST"), the
	name in Anton (its link covers the whole card), short lines under it, and
	the card's buttons, which sit above that link. Narrow, the buttons take
	the full width under the rest.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		kicker: string;
		title: string;
		href: string;
		/** Runs when the card is first pointed at or focused. */
		onintent?: () => void;
		art: Snippet;
		lines: Snippet;
		actions?: Snippet;
	}

	let { kicker, title, href, onintent, art, lines, actions }: Props = $props();
</script>

<Card
	variant="torn"
	padding="none"
	as="article"
	class="search-top"
	onpointerenter={onintent}
	onfocusin={onintent}
>
	<div class="search-top__body">
		<div class="search-top__art">{@render art()}</div>
		<div class="search-top__text">
			<p class="search-top__kicker">{kicker}</p>
			<h3 class="search-top__title">
				<a {href} class="search-top__link">{title}</a>
			</h3>
			<div class="search-top__lines">{@render lines()}</div>
		</div>
		{#if actions}
			<div class="search-top__actions">{@render actions()}</div>
		{/if}
	</div>
</Card>

<style>
	:global(.search-top) {
		container-type: inline-size;
		height: 100%;
	}

	.search-top__body {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: 1.25rem 1.5rem;
		height: 100%;
		padding: 1.75rem 1.75rem 1.75rem 1.5rem;
	}

	.search-top__art {
		position: relative;
		width: 8.5rem;
	}

	.search-top__kicker {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.24em;
		text-transform: uppercase;
	}

	.search-top__title {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		margin-top: 0.125rem;
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 2.75rem;
		line-height: 1;
		text-transform: uppercase;
		font-synthesis: none;
		overflow-wrap: anywhere;
	}

	/* the name's link covers the card; the buttons and cover controls sit above it */
	.search-top__link::after {
		content: '';
		position: absolute;
		inset: 0;
		z-index: 1;
	}
	.search-top__link:focus-visible {
		outline: none;
	}
	.search-top__link:focus-visible::after {
		outline: 3px solid var(--dn-paper-content);
		outline-offset: -1.25rem;
		border-radius: 0.375rem;
	}
	.search-top__link:hover {
		text-decoration: underline;
		text-decoration-thickness: 3px;
		text-underline-offset: 4px;
	}

	.search-top__lines {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.375rem;
		margin-top: 0.5rem;
		font-family: var(--font-mono);
		font-size: 0.9375rem;
	}

	.search-top__actions {
		position: relative;
		z-index: 2;
		display: flex;
		flex-direction: column;
		gap: 0.625rem;
		min-width: 13rem;
	}

	@container (max-width: 40rem) {
		.search-top__body {
			grid-template-columns: auto minmax(0, 1fr);
			padding: 1.5rem 1.25rem 1.375rem;
			gap: 1rem 1.125rem;
		}
		.search-top__art {
			width: 6.5rem;
		}
		.search-top__title {
			font-size: 2.25rem;
		}
		.search-top__actions {
			grid-column: 1 / -1;
			min-width: 0;
		}
	}
	@container (max-width: 20rem) {
		.search-top__art {
			width: 5rem;
		}
		.search-top__kicker {
			font-size: 0.6875rem;
			letter-spacing: 0.18em;
		}
		.search-top__title {
			font-size: 1.875rem;
		}
		.search-top__lines {
			font-size: 0.8125rem;
		}
	}
</style>
