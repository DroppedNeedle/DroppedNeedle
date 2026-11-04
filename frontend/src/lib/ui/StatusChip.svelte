<!--
	A short status or format label: FLAC, IN LIBRARY, PENDING. The words carry
	the meaning; the tone only backs them up. `action` is the acid label with
	dark ink, for what the library has; the others are a tinted chip in a
	status colour or plain.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { KitIcon } from './Button.svelte';

	interface Props {
		tone?: 'action' | 'neutral' | 'info' | 'success' | 'warning' | 'error';
		size?: 'xs' | 'sm';
		icon?: KitIcon;
		/** Layout only: margin, flex. */
		class?: string;
		children: Snippet;
	}

	let {
		tone = 'neutral',
		size = 'sm',
		icon: Icon,
		class: className = '',
		children
	}: Props = $props();

	// Whole class strings, so Tailwind's scan finds every one.
	const tones = {
		action: 'border-action-edge bg-action text-action-content',
		neutral: 'border-edge bg-base-200 text-base-content',
		info: 'border-info/50 bg-info/12 text-info',
		success: 'border-success/50 bg-success/12 text-success',
		warning: 'border-warning/50 bg-warning/12 text-warning',
		error: 'border-error/50 bg-error/12 text-error'
	};
	const sizes = { xs: 'h-5 px-1.5', sm: 'h-6 px-2' };
</script>

<span
	class="inline-flex shrink-0 items-center gap-1 rounded-full border text-xs leading-none font-bold tracking-wide whitespace-nowrap uppercase {sizes[
		size
	]} {tones[tone]} {className}"
>
	{#if Icon}<Icon class="size-3" aria-hidden="true" />{/if}
	{@render children()}
</span>
