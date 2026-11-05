<!--
	The one album tile and artist tile: the art, then the name and a mono line
	under it, on the page with no card around them. `kind="album"` is a square
	sleeve with an uppercase title; `kind="artist"` a round picture with the
	name centred.
	The art and the text are one target: a link with `href`, a button-like
	tile with `onclick`, or plain. `overlay` holds the tile's own badges,
	menus and buttons, placed by the caller over the art; it sits outside that
	target, so its controls never nest inside the link. `onintent` runs when
	the target is first pointed at or focused (to fetch more about it).
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		kind?: 'album' | 'artist';
		title: string;
		/** The heading level the title sits at on its page. */
		titleAs?: 'h2' | 'h3' | 'p';
		/** An artist's name may wrap to two lines (a dense grid); one by default. */
		wrapName?: boolean;
		href?: string;
		onclick?: () => void;
		/** The target's accessible name; the title when not given. */
		label?: string;
		onintent?: () => void;
		/** The picture, filling the art box. */
		art: Snippet;
		/** The line under the title: year, artist, counts. */
		meta?: Snippet;
		/** Anything below that, such as a play count. */
		footer?: Snippet;
		/** Badges and controls over the art, outside the link. */
		overlay?: Snippet;
		/** Layout only: width, flex. */
		class?: string;
	}

	let {
		kind = 'album',
		title,
		titleAs = 'h3',
		wrapName = false,
		href,
		onclick,
		label,
		onintent,
		art,
		meta,
		footer,
		overlay,
		class: className = ''
	}: Props = $props();

	const tag = $derived(href !== undefined ? 'a' : 'div');

	// A wrapped name breaks only between words: its longest word's width in
	// em (Inter bold, a little generous) lets that word shrink to fit.
	function wordEm(text: string): number {
		let widest = 0;
		for (const word of text.split(/\s+/)) {
			let em = 0;
			for (const char of word) {
				if ('mwMW'.includes(char)) em += 0.86;
				else if ("ilj.,'!|".includes(char)) em += 0.3;
				else if (char !== char.toLowerCase()) em += 0.66;
				else em += 0.57;
			}
			widest = Math.max(widest, em);
		}
		return widest * 1.02;
	}
	const fitWord = $derived(wrapName ? wordEm(title) : undefined);
	const interactive = $derived(href !== undefined || onclick !== undefined);

	function keydown(event: KeyboardEvent) {
		if (!onclick || href !== undefined) return;
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			onclick();
		}
	}
</script>

<div
	class="dn-tile group relative min-w-0 {className}"
	data-kind={kind}
	data-wrap={wrapName || undefined}
>
	<svelte:element
		this={tag}
		{href}
		class="dn-tile__target block rounded-lg {interactive ? 'cursor-pointer' : ''}"
		role={href === undefined && onclick ? 'button' : undefined}
		tabindex={href === undefined && onclick ? 0 : undefined}
		aria-label={interactive ? (label ?? title) : undefined}
		onclick={href === undefined ? onclick : undefined}
		onkeydown={keydown}
		onpointerenter={onintent}
		onfocus={onintent}
	>
		<div class="dn-tile__art relative aspect-square">
			{@render art()}
		</div>
		<div class="dn-tile__text">
			<svelte:element this={titleAs} class="dn-tile__title" style:--dn-tile-word={fitWord}
				>{title}</svelte:element
			>
			{#if meta}
				<div class="dn-tile__meta">{@render meta()}</div>
			{/if}
			{#if footer}
				<div class="mt-1.5">{@render footer()}</div>
			{/if}
		</div>
	</svelte:element>
	{#if overlay}
		<div class="dn-tile__overlay">
			{@render overlay()}
		</div>
	{/if}
</div>

<style>
	.dn-tile__art {
		transition: translate 150ms;
	}
	@media (hover: hover) and (prefers-reduced-motion: no-preference) {
		.dn-tile__target:is(a, [role='button']):hover .dn-tile__art {
			translate: 0 -1px;
		}
	}

	.dn-tile__text {
		padding: 0.625rem 0.25rem 0.25rem;
	}

	/* the title: Inter bold, two lines kept so a row's years line up */
	.dn-tile__title {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow: hidden;
		min-height: 2.5em;
		font-family: var(--font-sans);
		font-size: 0.875rem;
		font-weight: 700;
		line-height: 1.25;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		color: var(--color-base-content);
		overflow-wrap: anywhere;
	}

	.dn-tile__meta {
		margin-top: 0.25rem;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		line-height: 1.35;
		color: var(--color-muted);
		font-variant-numeric: tabular-nums;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	/* the overlay is the art's box, laid over it; only its controls take clicks */
	.dn-tile__overlay {
		position: absolute;
		inset: 0 0 auto 0;
		z-index: 20;
		aspect-ratio: 1;
		pointer-events: none;
	}
	.dn-tile__overlay > :global(*) {
		pointer-events: auto;
	}

	.dn-tile[data-kind='artist'] .dn-tile__art {
		width: 82%;
		margin-inline: auto;
		margin-top: 0.5rem;
	}
	.dn-tile[data-kind='artist'] .dn-tile__text {
		text-align: center;
	}
	.dn-tile[data-kind='artist'] .dn-tile__title {
		-webkit-line-clamp: 1;
		line-clamp: 1;
		min-height: 1.25em;
		text-transform: none;
		letter-spacing: 0;
		font-size: 0.9375rem;
	}
	.dn-tile[data-kind='artist'][data-wrap] .dn-tile__text {
		container-type: inline-size;
	}
	.dn-tile[data-kind='artist'][data-wrap] .dn-tile__title {
		-webkit-line-clamp: 2;
		line-clamp: 2;
		min-height: 2.5rem;
		font-size: min(0.875rem, 100cqi / var(--dn-tile-word, 1));
		line-height: 1.25rem;
		overflow-wrap: normal;
	}
</style>
