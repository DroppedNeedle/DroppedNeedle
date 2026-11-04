<!--
	A short status or format label: FLAC, IN LIBRARY, PENDING. The words carry
	the meaning; the tone only backs them up. `action` is the acid label with
	dark ink, for what the library has; the others are a tinted chip in a
	status colour or plain. `md` is a state that stands in a row of buttons
	(an album's In Library): their height, but a soft fill with no edge, so
	it never reads as one of them.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { KitIcon } from './Button.svelte';

	type Tone = 'action' | 'neutral' | 'info' | 'success' | 'warning' | 'error';

	interface Props {
		tone?: Tone;
		/** `md` stands in a row of buttons, at their height. */
		size?: 'xs' | 'sm' | 'md';
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
	const chips: Record<Tone, string> = {
		action: 'border-action-edge bg-action text-action-content',
		neutral: 'border-edge bg-base-200 text-base-content',
		info: 'border-info/50 bg-info/12 text-info',
		success: 'border-success/50 bg-success/12 text-success',
		warning: 'border-warning/50 bg-warning/12 text-warning',
		error: 'border-error/50 bg-error/12 text-error'
	};
	const fills: Record<Tone, string> = {
		action: 'bg-action text-action-content',
		neutral: 'bg-selected text-base-content',
		info: 'bg-info/15 text-info',
		success: 'bg-success/15 text-success',
		warning: 'bg-warning/15 text-warning',
		error: 'bg-error/15 text-error'
	};
	const sizes = {
		xs: 'h-5 rounded-full border px-1.5 text-xs font-bold tracking-wide uppercase',
		sm: 'h-6 rounded-full border px-2 text-xs font-bold tracking-wide uppercase',
		md: 'h-10 gap-1.5 rounded-field px-3.5 text-sm font-semibold [&_svg]:size-4'
	};
</script>

<span
	class="inline-flex shrink-0 items-center gap-1 leading-none whitespace-nowrap {sizes[
		size
	]} {size === 'md' ? fills[tone] : chips[tone]} {className}"
>
	{#if Icon}<Icon class="size-3" aria-hidden="true" />{/if}
	{@render children()}
</span>
