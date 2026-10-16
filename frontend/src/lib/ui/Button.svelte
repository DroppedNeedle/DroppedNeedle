<!--
	The one button. A variant is a look and a job; `class` is for layout only
	(gap, width, margin, flex), never colour or size. With `href` it is a link,
	with `as` a dropdown `summary` or a `label` for a control. It defaults to
	`type="button"`, so a form needs `type="submit"` on its submit button.
	Replaces the hand-written `btn btn-*` class sets.
-->
<script lang="ts" module>
	import type { ComponentType, SvelteComponent } from 'svelte';
	import type { IconProps } from 'lucide-svelte';

	/** A lucide icon component, e.g. `Trash2`. */
	export type KitIcon = ComponentType<SvelteComponent<IconProps>>;

	export type ButtonVariant =
		| 'neutral'
		| 'primary'
		| 'accent'
		| 'outline'
		| 'ghost'
		| 'danger'
		| 'danger-ghost'
		| 'warning'
		| 'info'
		| 'lastfm'
		| 'listenbrainz';

	export type ButtonSize = 'xs' | 'sm' | 'md' | 'lg';

	// Whole class strings, so Tailwind's scan finds every one.
	const buttonVariants: Record<ButtonVariant, string> = {
		neutral: 'btn',
		primary: 'btn btn-primary',
		accent: 'btn btn-accent',
		outline: 'btn btn-outline',
		ghost: 'btn btn-ghost',
		danger: 'btn btn-error',
		'danger-ghost': 'btn btn-ghost text-error',
		warning: 'btn btn-warning',
		info: 'btn btn-info',
		lastfm: 'btn btn-lastfm',
		listenbrainz: 'btn btn-listenbrainz'
	};

	const buttonSizes: Record<ButtonSize, string> = {
		xs: 'btn-xs',
		sm: 'btn-sm',
		md: '',
		lg: 'btn-lg'
	};
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';
	import type {
		HTMLAnchorAttributes,
		HTMLButtonAttributes,
		HTMLLabelAttributes
	} from 'svelte/elements';

	type Attributes = Omit<
		HTMLButtonAttributes & HTMLAnchorAttributes & HTMLLabelAttributes,
		'type' | 'children'
	>;

	interface Props extends Attributes {
		variant?: ButtonVariant;
		size?: ButtonSize;
		/** Fully rounded ends, for a text button that sits among chips. */
		pill?: boolean;
		/** The pressed or current state of a toggle-like button. */
		active?: boolean;
		/** Shows a spinner and blocks clicks. */
		loading?: boolean;
		icon?: KitIcon;
		iconPosition?: 'start' | 'end';
		/** Renders a link. */
		href?: string;
		/** Renders a dropdown's `summary` or a `label` (pass `for`) instead of a button. */
		as?: 'summary' | 'label';
		type?: 'button' | 'submit' | 'reset';
		ref?: HTMLElement | null;
		children?: Snippet;
	}

	let {
		variant = 'neutral',
		size = 'md',
		pill = false,
		active = false,
		loading = false,
		icon: Icon,
		iconPosition = 'start',
		href,
		as,
		type = 'button',
		disabled = false,
		ref = $bindable(null),
		class: className = '',
		children,
		...rest
	}: Props = $props();

	const iconSizes: Record<ButtonSize, string> = {
		xs: 'size-3.5',
		sm: 'size-4',
		md: 'size-4',
		lg: 'size-5'
	};
	const spinnerSizes: Record<ButtonSize, string> = {
		xs: 'loading loading-spinner loading-xs',
		sm: 'loading loading-spinner loading-sm',
		md: 'loading loading-spinner loading-sm',
		lg: 'loading loading-spinner loading-md'
	};

	const tag = $derived(href !== undefined ? 'a' : (as ?? 'button'));
	const blocked = $derived(Boolean(disabled) || loading);
	const classes = $derived(
		[
			buttonVariants[variant],
			buttonSizes[size],
			pill ? 'rounded-full' : '',
			active ? 'btn-active' : '',
			tag !== 'button' && blocked ? 'btn-disabled' : '',
			className
		]
			.filter(Boolean)
			.join(' ')
	);
</script>

<svelte:element
	this={tag}
	bind:this={ref}
	{...rest}
	class={classes}
	href={tag === 'a' && !blocked ? href : undefined}
	type={tag === 'button' ? type : undefined}
	disabled={tag === 'button' ? blocked : undefined}
	aria-disabled={tag !== 'button' && blocked ? true : undefined}
	aria-busy={loading || undefined}
>
	{#if loading}
		<span class={spinnerSizes[size]} aria-hidden="true"></span>
	{:else if Icon && iconPosition === 'start'}
		<Icon class={iconSizes[size]} aria-hidden="true" />
	{/if}
	{@render children?.()}
	{#if Icon && iconPosition === 'end'}
		<Icon class={iconSizes[size]} aria-hidden="true" />
	{/if}
</svelte:element>
