<!--
	A Downloads section's head: its name in wide mono caps, a quieter second
	part ("auto-retrying"), a count chip, and the section's own buttons at the
	right. With `ontoggle` the name is the disclosure button of a collapsed
	section (History, Quarantine), and `note` reads beside it.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ChevronRight } from 'lucide-svelte';

	interface Props {
		title: string;
		/** The quieter part after a dot. */
		aside?: string;
		count?: number | null;
		/** Mono words beside a collapsible title ("12 in your crate"). */
		note?: string;
		open?: boolean;
		/** Makes the title a disclosure button. */
		ontoggle?: () => void;
		actions?: Snippet;
	}

	let { title, aside, count = null, note, open = false, ontoggle, actions }: Props = $props();
</script>

{#snippet countChip()}
	{#if count != null}<span class="dl-head__count">{count}</span>{/if}
{/snippet}

<div class="dl-head">
	{#if ontoggle}
		<h2 class="min-w-0">
			<button
				type="button"
				class="dl-head__title dl-head__toggle"
				aria-expanded={open}
				onclick={ontoggle}
			>
				<ChevronRight
					class="dl-head__chevron {open ? 'rotate-90' : ''}"
					aria-hidden="true"
				/>{title}
				{#if note}<span class="dl-head__note">{note}</span>{/if}
				{@render countChip()}
			</button>
		</h2>
	{:else}
		<h2 class="dl-head__title min-w-0">
			<span
				>{title}{#if aside}{` · ${aside}`}{/if}</span
			>
			{@render countChip()}
		</h2>
	{/if}
	{#if actions}
		<div class="flex shrink-0 flex-wrap items-center justify-end gap-2">{@render actions()}</div>
	{/if}
</div>

<style>
	.dl-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem 1rem;
		margin-bottom: 0.875rem;
	}

	.dl-head__title {
		display: inline-flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.25rem 0.875rem;
		font-family: var(--font-mono);
		font-size: 1.1875rem;
		font-weight: 600;
		line-height: 1.3;
		letter-spacing: 0.26em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}

	.dl-head__toggle {
		gap: 0.25rem 0.75rem;
		border-radius: 0.25rem;
		text-align: left;
		transition: color 150ms;
	}
	.dl-head__toggle:hover {
		color: var(--dn-title);
	}
	.dl-head__toggle:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 3px;
	}
	.dl-head__toggle :global(.dl-head__chevron) {
		width: 1.125rem;
		height: 1.125rem;
		flex: none;
		stroke-width: 2.5;
		transition: rotate 150ms;
	}

	.dl-head__note {
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		color: var(--dn-muted);
	}

	.dl-head__count {
		display: inline-grid;
		place-items: center;
		min-width: 2rem;
		height: 1.5rem;
		padding-inline: 0.5rem;
		border-radius: 9999px;
		font-size: 0.8125rem;
		font-weight: 700;
		letter-spacing: 0;
		font-variant-numeric: tabular-nums;
		color: var(--color-base-content);
		background-color: var(--dn-selected);
		border: 1px solid var(--dn-line);
	}

	@media (max-width: 639px) {
		.dl-head__title {
			font-size: 0.9375rem;
			letter-spacing: 0.18em;
		}
		.dl-head__note {
			font-size: 0.75rem;
			letter-spacing: 0.1em;
		}
	}
</style>
