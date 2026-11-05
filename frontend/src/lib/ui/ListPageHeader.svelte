<!--
	A browse page's header: the huge Anton title, a thin rule, the count in
	bold italic mono (or a spaced mono subtitle under the title), the barcode
	ticket, and a big record with acid light at the top right. `backHref`
	adds the square back button before the title, or with `backStyle="link"`
	an underlined mono caps link over it ("BACK TO FOLLOWING"); `actions`
	holds the page's condensed caps buttons at the right. `eyebrow` is a spaced mono
	line over the title ("SEARCH RESULTS FOR"), `fitTitle` steps a long,
	typed title (a search) down so it wraps less, and `below` holds a row
	under the title (search's tab chips). `lede` is a plain sentence under
	the title that wraps on a phone, and `stroke` draws a short brush stroke
	after the title in place of the rule. `longSubtitle` sets a sentence-long
	subtitle smaller and lets it wrap on a phone; `snugTitle` keeps a wide
	one-word title on one line on a phone, sized to its column. `tagline` is
	a hub's short statement in spaced mono caps beside the title, underlined
	by an acid brush stroke; the ticket then sits on the title's lower right.
	The record, ticket and strokes are decoration.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ArrowLeft } from 'lucide-svelte';
	import BarcodeTicket from './BarcodeTicket.svelte';
	import VinylRecord from './VinylRecord.svelte';

	interface Props {
		title: string;
		/** A spaced mono line under the title (the hub). */
		subtitle?: string;
		/** The list's total, beside the title ("221 albums"). */
		count?: string | null;
		/** The ticket's mono label. */
		ticket: string;
		backHref?: string;
		backLabel?: string;
		/** The square button before the title, or a mono caps link over it. */
		backStyle?: 'button' | 'link';
		actions?: Snippet;
		/** A spaced mono line over the title. */
		eyebrow?: string;
		/** Step a long title down in size (a typed query). */
		fitTitle?: boolean;
		/** A row under the title, such as tab chips. */
		below?: Snippet;
		/** A plain sentence under the title (Inter, wraps). */
		lede?: string;
		/** A short brush stroke after the title, in place of the rule. */
		stroke?: boolean;
		/** A sentence-long subtitle: smaller, and it wraps on a phone. */
		longSubtitle?: boolean;
		/** A wide one-word title: on a phone it shrinks to fit rather than break. */
		snugTitle?: boolean;
		/** A hub's short statement beside the title. */
		tagline?: string;
	}

	let {
		title,
		subtitle,
		count = null,
		ticket,
		backHref,
		backLabel = 'Back',
		backStyle = 'button',
		actions,
		eyebrow,
		fitTitle = false,
		below,
		lede,
		stroke = false,
		longSubtitle = false,
		snugTitle = false,
		tagline
	}: Props = $props();

	const titleScale = $derived(
		!fitTitle || title.length <= 10
			? 1
			: title.length <= 18
				? 0.72
				: title.length <= 30
					? 0.54
					: 0.42
	);
</script>

<header
	class="dn-lph"
	class:dn-lph--back={backHref && backStyle === 'button'}
	class:dn-lph--actions={actions}
	class:dn-lph--snug={snugTitle}
	class:dn-lph--tagline={tagline}
	style:--dn-lph-scale={titleScale}
>
	<div class="dn-lph__row">
		{#if backHref && backStyle === 'button'}
			<a href={backHref} class="dn-lph__back" aria-label={backLabel} title={backLabel}>
				<ArrowLeft class="size-6" aria-hidden="true" />
			</a>
		{/if}
		<div class="dn-lph__titles">
			{#if backHref && backStyle === 'link'}
				<a href={backHref} class="dn-lph__backlink">
					<ArrowLeft class="size-4" aria-hidden="true" />{backLabel}
				</a>
			{/if}
			{#if eyebrow}<p class="dn-lph__eyebrow">{eyebrow}</p>{/if}
			{#if stroke}
				<div class="dn-lph__line">
					<h1 class="dn-lph__title">{title}</h1>
					<svg
						class="dn-decor dn-lph__stroke"
						viewBox="0 0 120 16"
						preserveAspectRatio="none"
						aria-hidden="true"
						><path
							d="M1 8.6C9 5.4 24 5.8 41 5.1 62 4.3 86 4.6 104 5.5l12 .6.9 1.4-9 .6c-14 .9-30 1.2-47 2.1C40 11.1 21 11.9 6 12.4 2.6 12.4.4 10.8 1 8.6Z"
						/><path d="M64 13.1c16-.6 34-1 50-.3l-.4 1c-17 .1-33 .3-49.6.1Z" /><path
							d="M92 2.6c8 .1 17 .3 25 1l-.6.9c-8-.4-16-.6-24.6-.8Z"
						/></svg
					>
				</div>
			{:else}
				<h1 class="dn-lph__title">{title}</h1>
			{/if}
			{#if subtitle}<p class="dn-lph__sub" class:dn-lph__sub--long={longSubtitle}>
					{subtitle}
				</p>{/if}
			{#if lede}<p class="dn-lph__lede">{lede}</p>{/if}
			{#if tagline}<BarcodeTicket label={ticket} class="dn-lph__ticket" />{/if}
		</div>
		{#if tagline}
			<p class="dn-lph__tagline">
				{tagline}
				<svg
					class="dn-decor dn-lph__brush"
					viewBox="0 0 120 8"
					preserveAspectRatio="none"
					aria-hidden="true"
				>
					<path
						d="M1 5.6C14 4.2 30 3.4 52 3.1S96 2.6 119 1.4L118.4 4.6C98 5.4 74 5.6 50 6.1S16 6.9 2.2 7.4Z"
					/>
				</svg>
			</p>
		{:else}
			{#if !stroke}<span class="dn-decor dn-lph__rule" aria-hidden="true"></span>{/if}
			<div class="dn-lph__meta">
				{#if count}<p class="dn-lph__count">{count}</p>{/if}
				<BarcodeTicket label={ticket} class="dn-lph__ticket" />
			</div>
		{/if}
		<div class="dn-lph__deck" aria-hidden="true">
			<VinylRecord sheen="acid" class="dn-lph__record" />
		</div>
		{#if actions}
			<div class="dn-lph__actions">{@render actions()}</div>
		{/if}
	</div>
	{#if below}
		<div class="dn-lph__below">{@render below()}</div>
	{/if}
</header>

<style>
	/* the header's edges cut the record, which hangs off its top */
	.dn-lph {
		--pad-top: 1.25rem;
		position: relative;
		isolation: isolate;
		overflow: clip;
		padding: var(--pad-top) 2rem 1.5rem;
	}

	/* The record takes the room between the ticket and the buttons (or the
	   page's edge), so it never sits under the title, ticket or buttons. */
	.dn-lph__deck {
		position: relative;
		z-index: -1;
		flex: 1 1 0;
		align-self: stretch;
		min-width: 0;
		margin-inline: -1rem;
		container-type: inline-size;
		pointer-events: none;
	}
	.dn-lph__deck :global(.dn-lph__record) {
		--size: min(100cqw - 0.5rem, 22rem);
		--sheen-from: 100deg;
		position: absolute;
		top: calc(-1 * var(--pad-top) - 0.42 * var(--size));
		left: 50%;
		width: var(--size);
		translate: -50% 0;
	}
	/* a list's record runs off the page's edge, its lower left in view */
	.dn-lph:not(.dn-lph--actions) .dn-lph__deck :global(.dn-lph__record) {
		--size: min(96cqw, 34rem);
		--sheen-from: 165deg;
		top: calc(-1 * var(--pad-top) - 0.64 * var(--size));
		left: auto;
		right: -24cqw;
		translate: none;
	}

	.dn-lph__row {
		display: flex;
		align-items: center;
		gap: 1.5rem;
	}

	.dn-lph__back {
		display: grid;
		flex: none;
		place-items: center;
		width: 3rem;
		height: 3rem;
		border: 2px solid var(--dn-title);
		border-radius: 0.5rem;
		color: var(--dn-title);
		transition: background-color 150ms;
	}
	.dn-lph__back:hover {
		background-color: color-mix(in oklab, var(--dn-title) 14%, transparent);
	}

	.dn-lph__titles {
		min-width: 0;
	}

	/* a hub's statement: the ticket pinned to the title's lower right, the
	   words in a narrow mono column beside it */
	.dn-lph--tagline .dn-lph__titles {
		position: relative;
		padding-right: 5.5rem;
		padding-bottom: 1.5rem;
	}
	.dn-lph--tagline .dn-lph__titles :global(.dn-lph__ticket) {
		position: absolute;
		right: 0;
		bottom: 0;
		width: 9.25rem;
	}
	.dn-lph__tagline {
		flex: none;
		align-self: flex-start;
		max-width: 15.5rem;
		margin-top: 1.25rem;
		font-family: var(--font-mono);
		font-size: 0.9375rem;
		font-weight: 600;
		line-height: 1.55;
		letter-spacing: 0.22em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
	.dn-lph__brush {
		display: block;
		width: 8rem;
		height: 0.5rem;
		margin-top: 0.75rem;
		fill: var(--dn-title);
	}
	.dn-lph__backlink {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		margin-bottom: 0.75rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
		text-decoration: underline;
		text-decoration-thickness: 1px;
		text-underline-offset: 0.4em;
		color: var(--color-base-content);
	}
	.dn-lph__backlink:hover {
		color: var(--dn-title);
	}

	.dn-lph__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: calc(6.5rem * var(--dn-lph-scale, 1));
		line-height: 0.95;
		letter-spacing: 0.005em;
		text-transform: uppercase;
		color: var(--dn-title);
		font-synthesis: none;
		overflow-wrap: anywhere;
	}
	.dn-lph:not(.dn-lph--back) .dn-lph__title {
		font-size: calc(7rem * var(--dn-lph-scale, 1));
	}

	.dn-lph__eyebrow {
		margin-bottom: 0.25rem;
		font-family: var(--font-mono);
		font-size: 1rem;
		font-weight: 600;
		letter-spacing: 0.3em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}

	.dn-lph__below {
		margin-top: 1rem;
	}

	.dn-lph__sub {
		margin-top: 0.375rem;
		font-family: var(--font-mono);
		font-size: 1rem;
		font-weight: 600;
		letter-spacing: 0.25em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}

	.dn-lph__line {
		display: flex;
		align-items: center;
		gap: 1.25rem;
	}
	.dn-lph__stroke {
		flex: none;
		width: 6.5rem;
		height: 1.125rem;
		margin-top: 1.5rem;
		fill: var(--dn-title);
	}

	.dn-lph__lede {
		margin-top: 0.5rem;
		max-width: 42ch;
		font-size: 1.125rem;
		line-height: 1.4;
		color: var(--color-base-content);
	}
	.dn-lph__sub--long {
		font-size: 0.875rem;
		letter-spacing: 0.2em;
	}

	.dn-lph__rule {
		flex: none;
		align-self: stretch;
		width: 2px;
		margin-block: 1.5rem 1rem;
		background-color: color-mix(in oklab, var(--dn-title) 70%, transparent);
	}

	.dn-lph__meta {
		display: flex;
		flex: none;
		flex-direction: column;
		gap: 0.75rem;
		padding-top: 0.5rem;
	}

	.dn-lph__count {
		font-family: var(--font-mono);
		font-size: 1.5rem;
		font-style: italic;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--color-base-content);
	}

	.dn-lph__meta :global(.dn-lph__ticket) {
		width: 10.25rem;
	}
	.dn-lph--back .dn-lph__meta :global(.dn-lph__ticket) {
		width: 12.5rem;
	}

	.dn-lph__actions {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 0.875rem;
		margin-left: auto;
		padding-bottom: 1.5rem;
	}

	@media (max-width: 1399px) {
		.dn-lph__title {
			font-size: calc(5rem * var(--dn-lph-scale, 1));
		}
		.dn-lph:not(.dn-lph--back) .dn-lph__title {
			font-size: calc(6rem * var(--dn-lph-scale, 1));
		}
		.dn-lph__row {
			flex-wrap: wrap;
		}
		.dn-lph__actions {
			flex-basis: 100%;
			justify-content: flex-start;
			margin-left: 0;
			padding-bottom: 0;
		}
	}

	@media (max-width: 1023px) {
		.dn-lph {
			padding-inline: 1.5rem;
		}
	}

	/* Phone: the ticket beside the title, the count or subtitle under it,
	   the buttons in one row below. */
	@media (max-width: 767px) {
		.dn-lph {
			--pad-top: 1rem;
			padding: var(--pad-top) 1rem 0.875rem;
		}
		.dn-lph__row {
			display: grid;
			grid-template-columns: auto minmax(0, 1fr) auto;
			grid-template-areas:
				'back title ticket'
				'back count ticket'
				'actions actions actions';
			align-items: center;
			gap: 0 0.75rem;
		}
		.dn-lph__back {
			grid-area: back;
			width: 2.5rem;
			height: 2.5rem;
		}
		.dn-lph__titles {
			grid-area: title;
		}
		.dn-lph:not(.dn-lph--back) .dn-lph__titles {
			grid-column: 1 / 3;
			grid-row: 1 / 3;
		}
		.dn-lph__meta {
			display: contents;
		}
		.dn-lph__count {
			grid-area: count;
			margin-top: 0.25rem;
			font-size: 0.875rem;
			font-style: normal;
			letter-spacing: 0.22em;
		}
		.dn-lph__meta :global(.dn-lph__ticket),
		.dn-lph--back .dn-lph__meta :global(.dn-lph__ticket) {
			grid-area: ticket;
			width: 6.5rem;
			margin-top: -0.75rem;
		}
		.dn-lph__title,
		.dn-lph:not(.dn-lph--back) .dn-lph__title {
			font-size: calc(clamp(2.75rem, 15vw, 3.75rem) * var(--dn-lph-scale, 1));
		}
		.dn-lph__backlink {
			margin-bottom: 0.5rem;
			font-size: 0.6875rem;
			letter-spacing: 0.16em;
		}
		.dn-lph__eyebrow {
			margin-bottom: 0.125rem;
			font-size: 0.75rem;
			letter-spacing: 0.22em;
		}
		.dn-lph__below {
			margin-top: 0.75rem;
		}
		.dn-lph--back .dn-lph__title {
			font-size: clamp(1.75rem, 9.6vw, 2.5rem);
			white-space: nowrap;
		}
		.dn-lph__sub {
			margin-top: 0.25rem;
			font-size: 0.6875rem;
			letter-spacing: 0.16em;
			white-space: nowrap;
		}
		.dn-lph--snug .dn-lph__titles {
			container-type: inline-size;
		}
		.dn-lph--snug .dn-lph__title,
		.dn-lph--snug:not(.dn-lph--back) .dn-lph__title {
			font-size: min(clamp(2.75rem, 15vw, 3.75rem), 21cqw);
			white-space: nowrap;
		}
		.dn-lph__sub--long {
			font-size: 0.625rem;
			line-height: 1.5;
			letter-spacing: 0.12em;
			white-space: normal;
		}
		.dn-lph__rule {
			display: none;
		}
		.dn-lph__stroke {
			display: none;
		}
		.dn-lph__lede {
			margin-top: 0.25rem;
			font-size: 0.875rem;
		}
		.dn-lph.dn-lph--tagline .dn-lph__titles {
			grid-column: 1 / 3;
			grid-row: 1;
			justify-self: start;
			padding-right: 3.5rem;
			padding-bottom: 0.875rem;
		}
		.dn-lph--tagline .dn-lph__titles :global(.dn-lph__ticket) {
			width: 6.25rem;
		}
		.dn-lph__tagline {
			grid-column: 1 / 3;
			grid-row: 2;
			max-width: none;
			margin-top: 1rem;
			font-size: 0.6875rem;
			line-height: 1.5;
			letter-spacing: 0.16em;
		}
		.dn-lph__brush {
			width: 5.5rem;
			margin-top: 0.5rem;
		}
		.dn-lph__actions {
			grid-area: actions;
			display: grid;
			grid-auto-columns: 1fr;
			grid-auto-flow: column;
			gap: 0.5rem;
			margin-top: 0.875rem;
		}
		.dn-lph__actions :global(.btn) {
			--btn-p: 0.375rem;
			--fontsize: 0.8125rem;
			--size: 2.25rem;
			gap: 0.375rem;
			white-space: nowrap;
		}
		.dn-lph__actions :global(.btn svg) {
			width: 1rem;
			height: 1rem;
		}
		/* no room beside the ticket: the record peeks out above and right of it */
		.dn-lph__deck {
			margin-inline: 0;
			grid-column: 3;
			grid-row: 1 / 3;
			align-self: stretch;
		}
		.dn-lph__deck :global(.dn-lph__record),
		.dn-lph:not(.dn-lph--actions) .dn-lph__deck :global(.dn-lph__record) {
			--size: 11rem;
			--sheen-from: 150deg;
			top: calc(-1 * var(--pad-top) - 6rem);
			left: auto;
			right: -3rem;
			translate: none;
		}
	}

	@media (max-width: 379px) {
		.dn-lph__meta :global(.dn-lph__ticket),
		.dn-lph--back .dn-lph__meta :global(.dn-lph__ticket) {
			display: none;
		}
		.dn-lph__actions :global(.btn) {
			--fontsize: 0.75rem;
			gap: 0.25rem;
		}
	}
</style>
