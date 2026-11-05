<!--
	One of the genre page's panels: a hairline frame round a head (an acid
	icon tile, the Anton title, a mono line under it) and its content.
	`torn` prints the head as a torn paper tab over the frame's top-left
	edge. With no `title` the head is a skeleton, for the loading state.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import Card from '$lib/ui/Card.svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';

	interface Props {
		title?: string;
		icon: KitIcon;
		/** The mono line under the title. */
		sub?: string;
		torn?: boolean;
		children: Snippet;
	}

	let { title, icon: Icon, sub, torn = false, children }: Props = $props();

	const headingId = $props.id();
</script>

{#snippet heading()}
	<div class="genre-panel__row">
		<span class="genre-panel__icon" aria-hidden="true"><Icon class="size-5" /></span>
		{#if title}
			<div class="genre-panel__text">
				<h2 id={headingId} class="genre-panel__title">{title}</h2>
				{#if sub}<p class="genre-panel__sub">{sub}</p>{/if}
			</div>
		{:else}
			<div class="min-w-0 space-y-2" aria-hidden="true">
				<div class="skeleton skeleton-shimmer h-6 w-48"></div>
				<div class="skeleton skeleton-shimmer h-3 w-28"></div>
			</div>
		{/if}
	</div>
{/snippet}

<section
	class="genre-panel"
	class:genre-panel--torn={torn}
	aria-labelledby={title ? headingId : undefined}
	aria-label={title ? undefined : 'Loading'}
	aria-busy={title ? undefined : true}
>
	<div class="genre-panel__head">
		{#if torn}
			<div class="genre-panel__lift">
				<Card variant="torn" padding="none" class="genre-panel__tab">
					{@render heading()}
				</Card>
			</div>
		{:else}
			{@render heading()}
		{/if}
	</div>
	{@render children()}
</section>

<style>
	.genre-panel {
		position: relative;
		display: flex;
		min-width: 0;
		flex-direction: column;
		padding: 1rem 0.875rem;
		border: 1px solid var(--dn-line);
		border-radius: 0.75rem;
		background-color: color-mix(in oklab, var(--color-base-200) 55%, transparent);
	}

	.genre-panel__head {
		display: flex;
		margin-bottom: 1.25rem;
	}

	.genre-panel__row {
		display: flex;
		min-width: 0;
		align-items: center;
		gap: 0.875rem;
	}

	.genre-panel__text {
		min-width: 0;
	}
	/* on the tab the counts run beside the title */
	.genre-panel--torn .genre-panel__text {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0.25rem 1.25rem;
	}
	.genre-panel--torn .genre-panel__sub {
		margin-top: 0;
	}

	/* The tab sits over the frame's top-left edge, a little askew. */
	.genre-panel--torn {
		margin-top: 1.75rem;
		padding-top: 0;
	}
	.genre-panel--torn .genre-panel__head {
		margin-top: -1.75rem;
	}
	/* the tab's shadow, on a wrapper since the tear's mask would cut it */
	.genre-panel__lift {
		min-width: 0;
		max-width: 100%;
		filter: drop-shadow(0 6px 10px rgb(0 0 0 / 0.28));
	}
	.genre-panel :global(.genre-panel__tab) {
		min-width: 0;
		max-width: 100%;
		padding: 1.125rem 1.25rem 1.125rem 0.875rem;
		rotate: -0.6deg;
	}

	.genre-panel__icon {
		display: grid;
		flex: none;
		place-items: center;
		width: 2.5rem;
		height: 2.5rem;
		border-radius: 0.5rem;
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: inset 0 0 0 1px var(--dn-action-edge);
	}
	/* on the paper tab the tile is printed in the paper's ink */
	.genre-panel--torn .genre-panel__icon {
		background-color: var(--dn-paper-content);
		color: var(--dn-paper);
		box-shadow: none;
	}

	.genre-panel__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 1.5rem;
		line-height: 1;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		overflow-wrap: anywhere;
		color: inherit;
		font-synthesis: none;
	}

	.genre-panel__sub {
		margin-top: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.18em;
		text-transform: uppercase;
	}

	@media (min-width: 640px) {
		.genre-panel {
			padding: 1.5rem;
		}
		.genre-panel--torn {
			padding-top: 0;
		}
		.genre-panel__title {
			font-size: 2rem;
		}
		.genre-panel :global(.genre-panel__tab) {
			padding-inline: 1rem 1.75rem;
		}
	}

	@media (forced-colors: active) {
		.genre-panel__lift {
			filter: none;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.genre-panel :global(.genre-panel__tab) {
			rotate: none;
		}
	}
</style>
