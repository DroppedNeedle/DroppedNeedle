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
		/** Let a long label wrap inside a narrow card instead of overflowing. */
		wrap?: boolean;
		icon?: KitIcon;
		/** Layout only: margin, flex. */
		class?: string;
		children: Snippet;
	}

	let {
		tone = 'neutral',
		size = 'sm',
		wrap = false,
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
		xs: 'border text-xs font-bold tracking-wide uppercase',
		sm: 'border text-xs font-bold tracking-wide uppercase',
		md: 'gap-1.5 text-sm font-semibold [&_svg]:size-4'
	};
	// One line: a pill at the size's height.
	const oneLine = {
		xs: 'h-5 rounded-full px-1.5',
		sm: 'h-6 rounded-full px-2',
		md: 'h-10 rounded-field px-3.5'
	};
	// Wrapped: softer corners and padding that keep every line inside the chip.
	const wrapped = {
		xs: 'rounded-xl px-2 py-1',
		sm: 'rounded-xl px-2.5 py-1',
		md: 'rounded-field px-3.5 py-2'
	};
	const layout = $derived(
		wrap
			? `min-w-0 max-w-full items-start whitespace-normal text-left leading-snug ${wrapped[size]}`
			: `shrink-0 items-center whitespace-nowrap leading-none ${oneLine[size]}`
	);
</script>

<span
	class="inline-flex gap-1 {layout} {sizes[size]} {size === 'md'
		? fills[tone]
		: chips[tone]} {className}"
>
	{#if Icon}<Icon class="size-3" aria-hidden="true" />{/if}
	{@render children()}
</span>
