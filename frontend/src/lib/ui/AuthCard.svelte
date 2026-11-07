<!--
	The one card of a signed-out page, inside AuthFrame: torn paper stock
	(cream in dark, paper-white in light, dark in midnight) with the sheet's
	theme inside, so every field, tick, alert and button in it is the one
	measured for that stock. Field labels print as bold caps and the fields
	take a dark ink edge. `title` is an Anton h2, `icon` a square ink tile
	beside it, `lead` a line under it. `error` is a red-tinted bar with an
	icon at the top of the card, announced as an alert. `tabs` holds folder
	tabs along the card's top edge: buttons with the `dn-auth-tab` class, the
	open one marked `aria-pressed="true"`, which prints acid and joins the
	paper. `actions` is the row of buttons at the foot.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { CircleAlert } from 'lucide-svelte';
	import type { KitIcon } from './Button.svelte';
	import Card from './Card.svelte';
	import { themeStore } from '$lib/stores/theme.svelte';

	interface Props {
		title?: string;
		icon?: KitIcon;
		lead?: string;
		error?: string | null;
		tabs?: Snippet;
		actions?: Snippet;
		/** Centres the title, the lead and the content (a short notice). */
		centred?: boolean;
		children?: Snippet;
	}

	let {
		title,
		icon: Icon,
		lead,
		error = null,
		tabs,
		actions,
		centred = false,
		children
	}: Props = $props();

	// A long form's error bar may be above the screen when it appears.
	function reveal(node: HTMLElement) {
		node.scrollIntoView({ block: 'nearest' });
	}
</script>

<div class="dn-auth-card" class:dn-auth-card--tabs={tabs} class:dn-auth-card--centred={centred}>
	{#if tabs}
		<div class="dn-auth-card__tabs">{@render tabs()}</div>
	{/if}
	<Card variant="torn" padding="none" class="dn-auth-card__stock">
		<div class="dn-auth-card__body" data-theme={themeStore.paper}>
			{#if error}
				<div class="dn-auth-card__error" role="alert" use:reveal>
					<CircleAlert class="size-5 shrink-0" aria-hidden="true" />
					<p>{error}</p>
				</div>
			{/if}
			{#if title || Icon || lead}
				<div class="dn-auth-card__head">
					{#if Icon}
						<span class="dn-auth-card__tile" aria-hidden="true"><Icon class="size-6" /></span>
					{/if}
					<div class="min-w-0">
						{#if title}<h2 class="dn-auth-card__title">{title}</h2>{/if}
						{#if lead}<p class="dn-auth-card__lead">{lead}</p>{/if}
					</div>
				</div>
			{/if}
			{@render children?.()}
			{#if actions}
				<div class="dn-auth-card__actions">{@render actions()}</div>
			{/if}
		</div>
	</Card>
</div>

<style>
	/* The card takes its shadow through the tear's mask from here. */
	.dn-auth-card {
		position: relative;
		filter: drop-shadow(0 18px 28px rgb(0 0 0 / 0.45));
	}
	:global([data-theme='light']) .dn-auth-card {
		filter: drop-shadow(0 14px 22px rgb(14 46 29 / 0.16));
	}

	.dn-auth-card :global(.dn-auth-card__stock) {
		position: relative;
		z-index: 1;
	}

	/* inside, the sheet's theme; its page colour is the card's own paper */
	.dn-auth-card__body {
		display: flex;
		flex-direction: column;
		gap: 1.125rem;
		padding: 2.25rem 2.25rem 2.5rem;
		background: transparent;
		color: var(--dn-paper-content);
		--dn-edge: color-mix(in oklab, var(--dn-paper-content) 78%, transparent);
	}

	.dn-auth-card__error {
		display: flex;
		align-items: flex-start;
		gap: 0.625rem;
		padding: 0.75rem 0.875rem;
		border: 1px solid color-mix(in oklab, var(--color-error) 55%, transparent);
		border-left-width: 4px;
		border-radius: 0.375rem;
		background: color-mix(in oklab, var(--color-error) 10%, transparent);
		color: var(--color-error);
		font-size: 0.875rem;
		font-weight: 600;
		line-height: 1.35;
	}
	.dn-auth-card__error p {
		padding-top: 0.0625rem;
		overflow-wrap: anywhere;
	}

	.dn-auth-card__head {
		display: flex;
		align-items: flex-start;
		gap: 1rem;
	}
	.dn-auth-card__tile {
		display: grid;
		flex: none;
		place-items: center;
		width: 3rem;
		height: 3rem;
		border-radius: 0.5rem;
		background: var(--dn-paper-tile);
		color: var(--dn-action);
	}
	.dn-auth-card__title {
		font-family: var(--font-display);
		font-size: 1.75rem;
		font-weight: 400;
		line-height: 1;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		color: var(--dn-paper-content);
	}
	.dn-auth-card__lead {
		margin-top: 0.5rem;
		font-size: 0.9375rem;
		line-height: 1.45;
		color: var(--dn-muted);
	}

	.dn-auth-card__actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.625rem;
		justify-content: flex-end;
		margin-top: 0.25rem;
	}

	.dn-auth-card--centred .dn-auth-card__body {
		align-items: center;
		text-align: center;
	}
	.dn-auth-card--centred .dn-auth-card__head {
		flex-direction: column;
		align-items: center;
	}
	.dn-auth-card--centred .dn-auth-card__actions {
		justify-content: center;
	}

	/* a section's heading inside the card (the upgrade's Found, Moved) */
	.dn-auth-card__body :global(section > h3) {
		margin-bottom: 0.625rem;
		padding-bottom: 0.375rem;
		border-bottom: 1px solid var(--dn-line);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
		color: var(--dn-paper-content);
	}

	/* Field labels in bold caps, fields with an ink edge. */
	.dn-auth-card__body :global(.fieldset) {
		padding-block: 0;
	}
	.dn-auth-card__body :global(.fieldset-legend) {
		padding-block: 0 0.375rem;
		font-size: 0.75rem;
		font-weight: 800;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--dn-paper-content);
	}
	.dn-auth-card__body :global(:is(.input, .select, .textarea)) {
		--input-color: var(--dn-edge);
		width: 100%;
		border-width: 1.5px;
		background-color: color-mix(in oklab, var(--color-base-200) 70%, transparent);
		color: var(--dn-paper-content);
	}
	.dn-auth-card__body :global(:is(.input, .select, .textarea):is(:focus, :focus-within)) {
		--input-color: var(--dn-paper-content);
	}
	.dn-auth-card__body :global(.join .dn-btn-neutral) {
		--btn-color: color-mix(in oklab, var(--color-base-200) 70%, transparent);
		--btn-border: var(--dn-edge);
		--btn-fg: var(--dn-paper-content);
		border-width: 1.5px;
	}
	.dn-auth-card__body :global(.checkbox:not(:checked)) {
		--input-color: var(--dn-edge);
	}
	.dn-auth-card__body :global(a:not(.btn)) {
		font-weight: 600;
		color: var(--dn-paper-content);
		text-decoration: underline;
		text-underline-offset: 0.2em;
	}
	.dn-auth-card__body :global(a:not(.btn):hover) {
		text-decoration-thickness: 2px;
	}

	/* Folder tabs: paper-tinted, tucked behind the card's torn edge; the
	   open one acid, in front, joined to the paper. */
	.dn-auth-card__tabs {
		position: relative;
		display: flex;
		gap: 0.25rem;
		padding-inline: 0.75rem;
		margin-bottom: -0.75rem;
	}
	.dn-auth-card__tabs :global(.dn-auth-tab) {
		position: relative;
		z-index: 0;
		display: inline-flex;
		flex: 1 1 auto;
		min-width: 0;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		height: 3.5rem;
		padding: 0 0.625rem 0.75rem;
		border-radius: 0.5rem 0.5rem 0 0;
		background: color-mix(in oklab, var(--dn-paper-content) 16%, var(--dn-paper));
		color: color-mix(in oklab, var(--dn-paper-content) 78%, var(--dn-paper));
		font-family: var(--font-display);
		font-size: 1rem;
		letter-spacing: 0.05em;
		line-height: 1;
		text-transform: uppercase;
		font-synthesis: none;
		cursor: pointer;
		transition:
			background-color 150ms,
			translate 150ms;
	}
	.dn-auth-card__tabs :global(.dn-auth-tab:hover) {
		background: color-mix(in oklab, var(--dn-paper-content) 8%, var(--dn-paper));
	}
	.dn-auth-card__tabs :global(.dn-auth-tab[aria-pressed='true']) {
		z-index: 2;
		height: 4.125rem;
		margin-top: -0.625rem;
		background: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: inset 0 0 0 1.5px var(--dn-action-edge);
	}
	.dn-auth-card__tabs :global(.dn-auth-tab:focus-visible) {
		z-index: 3;
		outline-offset: -4px;
		outline-color: var(--dn-paper-content);
	}
	.dn-auth-card__tabs :global(.dn-auth-tab svg) {
		flex: none;
		width: 1.125rem;
		height: 1.125rem;
	}

	@media (max-width: 639px) {
		.dn-auth-card__body {
			gap: 1rem;
			padding: 1.75rem 1.25rem 2rem;
		}
		.dn-auth-card__title {
			font-size: 1.5rem;
		}
		.dn-auth-card__actions > :global(*) {
			flex: 1 1 auto;
		}
		.dn-auth-card__tabs {
			padding-inline: 0.5rem;
		}
		/* icon-only on a phone; the label stays as the tab's name */
		.dn-auth-card__tabs :global(.dn-auth-tab__label) {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip-path: inset(50%);
			white-space: nowrap;
		}
		.dn-auth-card__tabs :global(.dn-auth-tab svg) {
			width: 1.375rem;
			height: 1.375rem;
		}
	}
</style>
