<!--
	A panel that groups one thing's content. `flat` sits on the page tint,
	`bordered` is a framed panel, `raised` a tile in a grid, `paper` a page's
	one light island (cream with dark ink in dark, raised paper-white in
	light), `torn` the same stock with its top and bottom edges ripped (a
	browse tile, a ticket), `acid` a torn sheet of the acid action colour with
	dark ink (a short static note beside a page's paper), `note` a callout
	with an action-coloured edge and a faint action tint (a hint beside a
	page's main actions), `index` a typed index card: a head line, a rule, then
	the body (a pick's reason at `size="sm"`, its evidence in a paper Modal at
	`size="lg"`); `lifted` raises it 4px, for a hover its parent reports. With
	`href` the whole card is one link named by its title, so it holds no other
	controls.
	A caller's `aria-*` attributes pass through, so a `section` card can be
	named by its own heading with `aria-labelledby`. `ref` binds the card's
	own element.
	Replaces the hand-written `card` blocks and the rounded panels doing a
	card's job. A heading or a row of buttons the props cannot hold uses
	CardTitle or CardActions inside the card.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';
	import CardActions from './CardActions.svelte';
	import CardTitle from './CardTitle.svelte';
	import { part } from './parts';

	interface Props extends Omit<HTMLAttributes<HTMLElement>, 'title' | 'children'> {
		variant?: 'flat' | 'bordered' | 'raised' | 'paper' | 'torn' | 'acid' | 'note' | 'index';
		/** Ignored by `index`, which pads itself by `size`. */
		padding?: 'none' | 'sm' | 'md' | 'lg';
		/** The index card's size: `sm` on a pick, `lg` in a paper Modal. */
		size?: 'sm' | 'lg';
		/** Raises the index card, while its parent's link is hovered. */
		lifted?: boolean;
		/** Rendered as an `h2`; use `header` for another heading or more than text. */
		title?: string;
		header?: Snippet;
		/** Buttons along the bottom edge. */
		actions?: Snippet;
		href?: string;
		as?: 'div' | 'article' | 'section';
		ref?: HTMLElement | null;
		children?: Snippet;
	}

	let {
		variant = 'flat',
		padding = 'lg',
		size = 'sm',
		lifted = false,
		title,
		header,
		actions,
		href,
		as = 'div',
		ref = $bindable(null),
		class: className = '',
		children,
		...rest
	}: Props = $props();

	const titleId = $props.id();

	const variants = {
		flat: 'card border border-base-300 bg-base-200',
		bordered: 'card border border-line bg-base-200',
		raised: 'card border border-line bg-base-200',
		paper: 'card dn-card-paper border border-paper-border bg-paper text-paper-content',
		torn: 'card dn-card-paper dn-card-torn bg-paper text-paper-content',
		acid: 'card dn-card-torn bg-action text-action-content',
		note: 'card border border-action-edge bg-action/10',
		index: 'card dn-card-index'
	};
	const linkHover = {
		flat: 'transition-colors duration-150 hover:bg-neutral',
		bordered: 'transition-colors duration-150 hover:border-edge hover:bg-neutral',
		raised: 'transition duration-150 hover:bg-neutral motion-safe:hover:-translate-y-px',
		paper: 'transition duration-150 hover:border-paper-content motion-safe:hover:-translate-y-px',
		torn: 'transition duration-150 motion-safe:hover:-translate-y-px',
		acid: 'transition duration-150 motion-safe:hover:-translate-y-px',
		note: 'transition-colors duration-150 hover:bg-action/20',
		index: ''
	};
	const paddings = {
		none: 'card-body p-0',
		sm: 'card-body p-3',
		md: 'card-body p-4',
		lg: 'card-body'
	};

	const indexPaddings = {
		sm: 'card-body gap-0 px-4 pt-2.5 pb-3',
		lg: 'card-body gap-0 px-5 pt-5 pb-6 sm:px-7.5 sm:pt-5.5'
	};
	const body = $derived(variant === 'index' ? indexPaddings[size] : paddings[padding]);

	const classes = $derived(
		[
			variants[variant],
			href !== undefined ? linkHover[variant] : '',
			href !== undefined ? 'focus-visible:outline-2 focus-visible:outline-focus' : '',
			className
		]
			.filter(Boolean)
			.join(' ')
	);
</script>

<svelte:element
	this={href !== undefined ? 'a' : as}
	bind:this={ref}
	{...rest}
	data-part={part('card')}
	{href}
	class={classes}
	data-size={variant === 'index' ? size : undefined}
	data-lifted={variant === 'index' && lifted ? '' : undefined}
	aria-labelledby={href !== undefined && title ? titleId : rest['aria-labelledby']}
>
	<div class={body}>
		{#if variant === 'index' && (header || title)}
			<div class="dn-card-index__head">
				{#if header}
					{@render header()}
				{:else}
					<CardTitle id={titleId}>{title}</CardTitle>
				{/if}
			</div>
		{:else if header}
			{@render header()}
		{:else if title}
			<CardTitle id={titleId}>{title}</CardTitle>
		{/if}
		{@render children?.()}
		{#if actions}
			<CardActions class="justify-end">{@render actions()}</CardActions>
		{/if}
	</div>
</svelte:element>
