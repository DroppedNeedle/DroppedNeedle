<!--
	A panel that groups one thing's content. `flat` sits on the page tint,
	`bordered` is a framed panel, `raised` a tile in a grid. With `href` the
	whole card is one link named by its title, so it holds no other controls.
	A caller's `aria-*` attributes pass through, so a `section` card can be
	named by its own heading with `aria-labelledby`.
	Replaces the hand-written `card` blocks and the rounded panels doing a
	card's job. A heading or a row of buttons the props cannot hold uses
	CardTitle or CardActions inside the card.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';
	import CardActions from './CardActions.svelte';
	import CardTitle from './CardTitle.svelte';

	interface Props extends Omit<HTMLAttributes<HTMLElement>, 'title' | 'children'> {
		variant?: 'flat' | 'bordered' | 'raised';
		padding?: 'none' | 'sm' | 'md' | 'lg';
		/** Rendered as an `h2`; use `header` for another heading or more than text. */
		title?: string;
		header?: Snippet;
		/** Buttons along the bottom edge. */
		actions?: Snippet;
		href?: string;
		as?: 'div' | 'article' | 'section';
		children?: Snippet;
	}

	let {
		variant = 'flat',
		padding = 'lg',
		title,
		header,
		actions,
		href,
		as = 'div',
		class: className = '',
		children,
		...rest
	}: Props = $props();

	const titleId = $props.id();

	const variants = {
		flat: 'card bg-base-200',
		bordered: 'card border border-base-300 bg-base-200',
		raised: 'card bg-base-100 shadow-sm'
	};
	const linkHover = {
		flat: 'transition-colors hover:bg-base-300',
		bordered: 'transition-colors hover:border-primary/40',
		raised: 'transition hover:shadow-md motion-safe:hover:-translate-y-0.5'
	};
	const paddings = {
		none: 'card-body p-0',
		sm: 'card-body p-3',
		md: 'card-body p-4',
		lg: 'card-body'
	};

	const classes = $derived(
		[
			variants[variant],
			href !== undefined ? linkHover[variant] : '',
			href !== undefined ? 'focus-visible:outline-2 focus-visible:outline-primary' : '',
			className
		]
			.filter(Boolean)
			.join(' ')
	);
</script>

<svelte:element
	this={href !== undefined ? 'a' : as}
	{...rest}
	{href}
	class={classes}
	aria-labelledby={href !== undefined && title ? titleId : rest['aria-labelledby']}
>
	<div class={paddings[padding]}>
		{#if header}
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
