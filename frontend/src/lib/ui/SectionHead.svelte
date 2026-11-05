<!--
	A shelf's heading row: the Anton title, an optional chip beside it (a
	source, a date), and "See all" (or `moreLabel`) at the right when the
	shelf has a page of its own. With `href` the title is a link too. `actions` sits before
	"See all" for a shelf's own buttons or outside link. `icon` marks the
	title (a lock on a private shelf).
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ArrowRight } from 'lucide-svelte';
	import type { KitIcon } from './Button.svelte';

	interface Props {
		title: string;
		/** A mark before the title. */
		icon?: KitIcon;
		href?: string | null;
		/** The heading level on its page. */
		as?: 'h2' | 'h3';
		/** Chips beside the title. */
		chips?: Snippet;
		/** Controls before "See all". */
		actions?: Snippet;
		/** The link's words at the right; "See all" by default. */
		moreLabel?: string;
		/** Layout only: margin. */
		class?: string;
	}

	let {
		title,
		icon: Icon,
		href = null,
		as = 'h2',
		chips,
		actions,
		moreLabel = 'See all',
		class: className = ''
	}: Props = $props();
</script>

<div class="dn-section-head {className}">
	<div class="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1">
		<svelte:element this={as} class="dn-section-head__title">
			{#if Icon}<Icon class="dn-section-head__icon" aria-hidden="true" />{/if}
			{#if href}
				<a {href} class="dn-section-head__link">{title}</a>
			{:else}
				{title}
			{/if}
		</svelte:element>
		{@render chips?.()}
	</div>
	{#if actions || href}
		<div class="flex shrink-0 items-center gap-2">
			{@render actions?.()}
			{#if href}
				<a {href} class="dn-section-head__all">
					{moreLabel}<span class="sr-only">: {title}</span>
					<ArrowRight class="size-3.5" aria-hidden="true" />
				</a>
			{/if}
		</div>
	{/if}
</div>

<style>
	.dn-section-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		margin-bottom: 0.75rem;
	}

	.dn-section-head__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 1.625rem;
		line-height: 1;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		color: var(--color-base-content);
		font-synthesis: none;
	}
	@media (min-width: 640px) {
		.dn-section-head__title {
			font-size: 1.875rem;
		}
	}

	.dn-section-head__title :global(.dn-section-head__icon) {
		display: inline-block;
		width: 0.8em;
		height: 0.8em;
		margin-right: 0.3em;
		vertical-align: -0.04em;
		stroke-width: 2.5;
	}

	.dn-section-head__link {
		border-radius: 0.25rem;
		transition: color 150ms;
	}
	.dn-section-head__link:hover {
		color: var(--dn-title);
	}

	.dn-section-head__all {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		min-height: 2rem;
		padding-inline: 0.25rem;
		border-radius: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--color-base-content);
		transition: color 150ms;
	}
	.dn-section-head__all:hover {
		color: var(--dn-title);
	}
</style>
