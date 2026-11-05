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
		| 'caps'
		| 'danger'
		| 'danger-ghost'
		| 'warning'
		| 'info'
		| 'lastfm'
		| 'listenbrainz'
		| 'spotify';

	/** `display` and `display-sm` are condensed caps, for a page header's or a band's buttons. */
	export type ButtonSize = 'xs' | 'sm' | 'md' | 'lg' | 'display' | 'display-sm';

	// Whole class strings, so Tailwind's scan finds every one. The dn-btn-*
	// looks live in kit.css: primary is the acid action fill in both themes,
	// neutral and outline a solid surface with an edge, caps a quiet mono
	// uppercase link-like button (Back to top), and the service
	// variants (lastfm, listenbrainz, spotify) that service's own colour.
	const buttonVariants: Record<ButtonVariant, string> = {
		neutral: 'btn dn-btn-neutral',
		primary: 'btn dn-btn-action',
		accent: 'btn btn-accent',
		outline: 'btn dn-btn-outline',
		ghost: 'btn btn-ghost dn-btn-ghost',
		caps: 'btn btn-ghost dn-btn-ghost dn-btn-caps',
		danger: 'btn btn-error',
		'danger-ghost': 'btn btn-ghost dn-btn-ghost text-error',
		warning: 'btn btn-warning',
		info: 'btn btn-info',
		lastfm: 'btn btn-lastfm',
		listenbrainz: 'btn btn-listenbrainz',
		spotify: 'btn dn-btn-spotify'
	};

	const buttonSizes: Record<ButtonSize, string> = {
		xs: 'btn-xs',
		sm: 'btn-sm',
		md: 'dn-btn-md',
		lg: 'btn-lg',
		display: 'dn-btn-display',
		'display-sm': 'dn-btn-display-sm'
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
		/** Shown in place of the label while loading, e.g. "Saving...". */
		loadingLabel?: string;
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
		loadingLabel,
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
		lg: 'size-5',
		display: 'size-5',
		'display-sm': 'size-4'
	};
	const spinnerSizes: Record<ButtonSize, string> = {
		xs: 'loading loading-spinner loading-xs',
		sm: 'loading loading-spinner loading-sm',
		md: 'loading loading-spinner loading-sm',
		lg: 'loading loading-spinner loading-md',
		display: 'loading loading-spinner loading-sm',
		'display-sm': 'loading loading-spinner loading-sm'
	};

	const tag = $derived(href !== undefined ? 'a' : (as ?? 'button'));
	const blocked = $derived(Boolean(disabled) || loading);
	// With no icon to swap for the spinner, the spinner sits over the label,
	// which stays (unseen) so the button keeps its width and its name.
	const overlay = $derived(loading && !Icon && !loadingLabel);
	const classes = $derived(
		[
			buttonVariants[variant],
			buttonSizes[size],
			pill ? 'rounded-full' : '',
			active ? 'btn-active' : '',
			loading ? 'dn-btn-loading' : '',
			overlay ? 'relative' : '',
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
	aria-disabled={rest['aria-disabled'] ?? (tag !== 'button' && blocked ? true : undefined)}
	aria-busy={rest['aria-busy'] ?? (loading || undefined)}
>
	{#if overlay}
		<span class="absolute inset-0 grid place-items-center" aria-hidden="true">
			<span class={spinnerSizes[size]}></span>
		</span>
		<span class="contents text-transparent">{@render children?.()}</span>
	{:else}
		{#if loading && (iconPosition === 'start' || !Icon)}
			<span class={spinnerSizes[size]} aria-hidden="true"></span>
		{:else if Icon && iconPosition === 'start'}
			<Icon class={iconSizes[size]} aria-hidden="true" />
		{/if}
		{#if loading && loadingLabel}
			{loadingLabel}
		{:else}
			{@render children?.()}
		{/if}
		{#if Icon && iconPosition === 'end'}
			{#if loading}
				<span class={spinnerSizes[size]} aria-hidden="true"></span>
			{:else}
				<Icon class={iconSizes[size]} aria-hidden="true" />
			{/if}
		{/if}
	{/if}
</svelte:element>
