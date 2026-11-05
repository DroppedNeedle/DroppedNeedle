<!--
	One card of the Following hub: an icon, the Anton title with its count
	and "See all" in one head link that stretches over the whole card, so
	the card is one large click target. Links and buttons in the body take
	`dn-follow-over` to sit above it.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ArrowRight } from 'lucide-svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';

	interface Props {
		title: string;
		/** Shown in brackets after the title; left out while unknown. */
		count?: number | null;
		href: string;
		icon: KitIcon;
		/** Layout only: grid placement. */
		class?: string;
		children: Snippet;
	}

	let { title, count = null, href, icon: Icon, class: className = '', children }: Props = $props();

	const headingId = $props.id();
</script>

<section class="fp {className}" aria-labelledby={headingId}>
	<a {href} class="fp__head">
		<Icon class="fp__icon" aria-hidden="true" />
		<h2 id={headingId} class="fp__title">
			{title}{#if count != null}<span class="fp__count">&nbsp;({count})</span>{/if}
		</h2>
		<span class="fp__all">
			<span class="fp__all-words">See all</span>
			<ArrowRight class="size-4" aria-hidden="true" />
		</span>
	</a>
	<div class="fp__body">
		{@render children()}
	</div>
</section>

<style>
	.fp {
		position: relative;
		display: flex;
		flex-direction: column;
		min-width: 0;
		border: 1px solid var(--dn-line);
		border-radius: 0.875rem;
		background-color: color-mix(in oklab, var(--color-base-200) 82%, transparent);
		transition: border-color 150ms;
	}
	.fp:hover {
		border-color: color-mix(in oklab, var(--dn-title) 70%, transparent);
	}
	.fp:has(.fp__head:focus-visible) {
		outline: 2px solid var(--dn-focus);
		outline-offset: 2px;
	}

	.fp__head {
		display: flex;
		align-items: center;
		gap: 0.875rem;
		padding: 1rem 1.25rem 0.875rem;
		border-bottom: 1px solid var(--dn-line);
		color: var(--dn-title);
		outline: none;
	}
	/* the head link covers the card; the body's own controls sit above it */
	.fp__head::after {
		content: '';
		position: absolute;
		inset: 0;
		z-index: 1;
		border-radius: inherit;
	}
	.fp :global(.dn-follow-over) {
		position: relative;
		z-index: 2;
	}

	.fp__head :global(.fp__icon) {
		flex: none;
		width: 1.625rem;
		height: 1.625rem;
		stroke-width: 1.75;
	}

	.fp__title {
		flex: 1;
		min-width: 0;
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 1.75rem;
		line-height: 1.05;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.fp__count {
		white-space: nowrap;
	}

	.fp__all {
		display: inline-flex;
		flex: none;
		align-items: center;
		gap: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 700;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		white-space: nowrap;
	}
	.fp__all :global(svg) {
		transition: translate 150ms;
	}
	.fp:hover .fp__all :global(svg) {
		translate: 0.1875rem 0;
	}

	.fp__body {
		display: flex;
		flex: 1;
		flex-direction: column;
		min-height: 0;
		padding: 1rem;
	}

	@media (max-width: 639px) {
		.fp__head {
			gap: 0.625rem;
			padding: 0.875rem 1rem 0.75rem;
		}
		.fp__head :global(.fp__icon) {
			width: 1.25rem;
			height: 1.25rem;
		}
		.fp__title {
			font-size: 1.3125rem;
		}
		.fp__all {
			font-size: 0.6875rem;
		}
		.fp__body {
			padding: 0.75rem;
		}
	}

	/* a narrow phone keeps the arrow; the words stay for screen readers */
	@media (max-width: 399px) {
		.fp__title {
			font-size: 1.1875rem;
		}
		.fp__all-words {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip-path: inset(50%);
			white-space: nowrap;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.fp__all :global(svg) {
			transition: none;
		}
	}
</style>
