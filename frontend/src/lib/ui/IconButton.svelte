<!--
	A button that shows only an icon. `label` is its accessible name and its
	tooltip, so it is required.
	`image` shows a picture (an avatar) in place of the icon, cropped to the
	button's shape and decorative, since `label` names the button.
	Replaces the hand-written `btn-circle` and `btn-square` buttons.
-->
<script lang="ts">
	import type { ComponentProps } from 'svelte';
	import Button, { type ButtonSize, type KitIcon } from './Button.svelte';

	interface Props extends Omit<
		ComponentProps<typeof Button>,
		'icon' | 'iconPosition' | 'pill' | 'children' | 'aria-label' | 'title'
	> {
		label: string;
		icon: KitIcon;
		/** A picture shown in place of the icon, which stays as its fallback. */
		image?: string | null;
		shape?: 'circle' | 'square';
	}

	let {
		label,
		icon: Icon,
		image,
		variant = 'ghost',
		size = 'md',
		shape = 'circle',
		ref = $bindable(null),
		class: className = '',
		...rest
	}: Props = $props();

	const shapes = { circle: 'btn-circle', square: 'btn-square' };
	const iconSizes: Record<ButtonSize, string> = {
		xs: 'size-3.5',
		sm: 'size-4',
		md: 'size-5',
		lg: 'size-6'
	};
	const imageSizes: Record<ButtonSize, string> = {
		xs: 'size-4',
		sm: 'size-6',
		md: 'size-7',
		lg: 'size-9'
	};
</script>

<Button
	{...rest}
	bind:ref
	{variant}
	{size}
	class="{shapes[shape]} {className}"
	aria-label={label}
	title={label}
>
	{#if !rest.loading}
		{#if image}
			<img
				src={image}
				alt=""
				class="{imageSizes[size]} {shape === 'circle' ? 'rounded-full' : 'rounded-sm'} object-cover"
			/>
		{:else}
			<Icon class={iconSizes[size]} aria-hidden="true" />
		{/if}
	{/if}
</Button>
