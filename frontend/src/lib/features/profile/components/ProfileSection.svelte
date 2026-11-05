<!--
	One profile section as a wide row: a paper label on the left (icon, title,
	one short line) with a torn right edge, the content on the page to its
	right. A section folds shut: closed, it shows only its one-line summary,
	and the whole row opens it. On a phone the label sits over the content.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ChevronDown, ChevronUp } from 'lucide-svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		id: string;
		title: string;
		/** The line under the title on the paper label. */
		blurb: string;
		icon?: KitIcon;
		/** A mark the icon set lacks (a service's logo), handed the size class. */
		glyph?: Snippet<[string]>;
		open: boolean;
		ontoggle: (open: boolean) => void;
		/** What the closed row says: counts and states, in mono caps. */
		summary?: Snippet;
		children: Snippet;
	}

	let { id, title, blurb, icon: Icon, glyph, open, ontoggle, summary, children }: Props = $props();

	const bodyId = $derived(`${id}-body`);
</script>

<section {id} class="pf-sec" class:pf-sec--open={open} aria-labelledby="{id}-title">
	<div class="pf-sec__label">
		<span class="pf-sec__mark" aria-hidden="true">
			{#if glyph}
				{@render glyph('size-full')}
			{:else if Icon}
				<Icon class="size-full" strokeWidth={1.75} />
			{/if}
		</span>
		<div class="min-w-0">
			<h2 class="pf-sec__title" id="{id}-title">
				<button
					type="button"
					class="pf-sec__toggle"
					aria-expanded={open}
					aria-controls={bodyId}
					onclick={() => ontoggle(!open)}>{title}</button
				>
			</h2>
			{#if open}<p class="pf-sec__blurb">{blurb}</p>{/if}
		</div>
		{#if open}
			<ChevronUp class="pf-sec__label-chevron" aria-hidden="true" />
		{/if}
	</div>

	{#if open}
		<div class="pf-sec__body" id={bodyId}>
			<IconButton
				label="Collapse {title}"
				icon={ChevronUp}
				size="sm"
				class="pf-sec__fold"
				onclick={() => ontoggle(false)}
			/>
			{@render children()}
		</div>
	{:else}
		<div class="pf-sec__summary" id={bodyId}>
			<div class="pf-sec__summary-text">{@render summary?.()}</div>
			<ChevronDown class="pf-sec__chevron" aria-hidden="true" />
		</div>
	{/if}
</section>

<style>
	.pf-sec {
		--label-w: 18rem;
		--tear: 1.5rem;
		position: relative;
		display: grid;
		grid-template-columns: var(--label-w) minmax(0, 1fr);
		border: 1px solid var(--dn-line);
		border-radius: 0.5rem;
		background-color: var(--color-base-100);
		scroll-margin-top: 6rem;
	}
	.pf-sec:not(.pf-sec--open) {
		--tear: 1.25rem;
	}
	.pf-sec:not(.pf-sec--open):hover {
		border-color: var(--dn-edge);
	}

	/* the paper label, its right edge torn through a mask */
	.pf-sec__label {
		position: relative;
		display: flex;
		align-items: flex-start;
		gap: 0.875rem;
		margin: -1px 0 -1px -1px;
		padding: 1rem calc(var(--tear) + 0.25rem) 1.125rem 1rem;
		border-radius: 0.5rem 0 0 0.5rem;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		--tear-edge: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 240' preserveAspectRatio='none'%3E%3Cpath d='M0 0L13 0L16 9L11 15L15 24L9 33L14 41L10 50L17 58L12 67L18 76L11 85L16 93L9 102L15 111L12 119L19 128L13 137L17 146L10 155L16 163L11 172L18 181L12 190L16 198L9 207L14 216L11 225L17 233L13 240L0 240Z'/%3E%3C/svg%3E");
		mask-image: linear-gradient(#000 0 0), var(--tear-edge);
		mask-size:
			calc(100% - var(--tear) + 1px) 100%,
			var(--tear) 15rem;
		mask-position:
			0 0,
			100% 0;
		mask-repeat: no-repeat, repeat-y;
		-webkit-mask-image: linear-gradient(#000 0 0), var(--tear-edge);
		-webkit-mask-size:
			calc(100% - var(--tear) + 1px) 100%,
			var(--tear) 15rem;
		-webkit-mask-position:
			0 0,
			100% 0;
		-webkit-mask-repeat: no-repeat, repeat-y;
	}
	.pf-sec:not(.pf-sec--open) .pf-sec__label {
		align-items: center;
		padding-block: 0.5rem;
	}

	.pf-sec__mark {
		flex: none;
		width: 2.25rem;
		height: 2.25rem;
		margin-top: 0.125rem;
	}
	.pf-sec:not(.pf-sec--open) .pf-sec__mark {
		width: 1.75rem;
		height: 1.75rem;
		margin-top: 0;
	}

	.pf-sec__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 2.25rem;
		line-height: 0.95;
		letter-spacing: 0.005em;
		text-transform: uppercase;
		font-synthesis: none;
		overflow-wrap: break-word;
	}
	.pf-sec:not(.pf-sec--open) .pf-sec__title {
		font-size: 1.625rem;
		line-height: 1.05;
	}

	.pf-sec__toggle {
		text-align: left;
		text-transform: inherit;
		cursor: pointer;
	}
	.pf-sec__toggle:focus-visible {
		outline: none;
	}
	/* the button covers the label; closed, the whole row */
	.pf-sec__toggle::after {
		content: '';
		position: absolute;
		inset: 0;
		z-index: 1;
		border-radius: inherit;
	}
	.pf-sec:not(.pf-sec--open) .pf-sec__label {
		position: static;
	}
	.pf-sec:not(.pf-sec--open) .pf-sec__toggle::after {
		border-radius: 0.5rem;
	}
	.pf-sec:has(.pf-sec__toggle:focus-visible) {
		outline: 3px solid var(--dn-focus);
		outline-offset: 2px;
	}

	.pf-sec__blurb {
		margin-top: 0.625rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		line-height: 1.45;
		color: var(--dn-paper-content);
	}

	.pf-sec__label :global(.pf-sec__label-chevron) {
		display: none;
	}

	.pf-sec__body {
		position: relative;
		min-width: 0;
		padding: 1.125rem 3.75rem 1.25rem 1.5rem;
	}
	.pf-sec__body :global(.pf-sec__fold) {
		position: absolute;
		top: 0.75rem;
		right: 0.75rem;
	}

	.pf-sec__summary {
		display: flex;
		align-items: center;
		gap: 1rem;
		min-width: 0;
		padding: 0.5rem 1.25rem 0.5rem 1.25rem;
	}
	.pf-sec__summary-text {
		display: flex;
		flex: 1 1 auto;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 1rem;
		min-width: 0;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.18em;
		text-transform: uppercase;
	}
	.pf-sec__summary :global(.pf-sec__chevron) {
		flex: none;
		width: 1.5rem;
		height: 1.5rem;
		color: var(--color-base-content);
	}

	/* tablet and phone: the label over the content, its bottom edge torn */
	@media (max-width: 767px) {
		.pf-sec,
		.pf-sec:not(.pf-sec--open) {
			grid-template-columns: minmax(0, 1fr);
			--tear: 0.875rem;
		}
		.pf-sec__label,
		.pf-sec:not(.pf-sec--open) .pf-sec__label {
			margin: -1px -1px 0;
			padding: 0.875rem 3.5rem calc(var(--tear) + 0.75rem) 1rem;
			border-radius: 0.5rem 0.5rem 0 0;
			--tear-edge: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 240 12' preserveAspectRatio='none'%3E%3Cpath d='M0 0L0 6L9 9.1L13 6.3L19 8.9L27 6.1L33 6.6L40 5.7L44 8.6L51 6.5L54 6.6L60 5L68 10.2L74 5L81 6.8L87 5.1L95 8.6L100 5.6L105 10L110 8.2L115 4.9L121 6.3L129 10.6L134 7.4L140 8.3L144 5.1L147 8.6L152 6.7L161 7.7L165 8.4L170 8.6L176 8.8L182 7L186 5.1L192 5.4L199 6.9L205 7.7L212 4.9L217 6.4L222 7.2L227 8.9L232 5.8L240 7L240 0Z'/%3E%3C/svg%3E");
			mask-size:
				100% calc(100% - var(--tear) + 1px),
				15rem var(--tear);
			mask-position:
				0 0,
				0 100%;
			mask-repeat: no-repeat, repeat-x;
			-webkit-mask-size:
				100% calc(100% - var(--tear) + 1px),
				15rem var(--tear);
			-webkit-mask-position:
				0 0,
				0 100%;
			-webkit-mask-repeat: no-repeat, repeat-x;
		}
		.pf-sec:not(.pf-sec--open) .pf-sec__label {
			padding-block: 0.625rem calc(var(--tear) + 0.375rem);
		}
		.pf-sec__title {
			font-size: 1.875rem;
		}
		.pf-sec:not(.pf-sec--open) .pf-sec__title {
			font-size: 1.5rem;
		}
		.pf-sec__blurb {
			margin-top: 0.375rem;
			font-size: 0.75rem;
		}
		.pf-sec__body {
			padding: 0.75rem 1rem 1.125rem;
		}
		.pf-sec__body :global(.pf-sec__fold) {
			display: none;
		}
		.pf-sec__label :global(.pf-sec__label-chevron) {
			display: block;
			position: absolute;
			top: 1rem;
			right: 1rem;
			width: 1.5rem;
			height: 1.5rem;
		}
		.pf-sec__summary {
			padding: 0.625rem 1rem 0.75rem;
		}
		.pf-sec__summary-text {
			font-size: 0.6875rem;
			letter-spacing: 0.14em;
		}
	}

	@media (forced-colors: active) {
		.pf-sec__label {
			mask-image: none;
			-webkit-mask-image: none;
			border: 1px solid CanvasText;
		}
	}
</style>
