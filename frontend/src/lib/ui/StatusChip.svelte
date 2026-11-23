<!--
	A short status or format label: FLAC, IN LIBRARY, PENDING. The words carry
	the meaning; the tone only backs them up. `action` is the acid label with
	dark ink, for what the library has; the others are a tinted chip in a
	status colour or plain. `md` is a state that stands in a row of buttons
	(an album's In Library): their height, but a soft fill with no edge, so
	it never reads as one of them.
	A `look` is what became of a pick, in its own word and mark: `kept` (acid,
	a tick), `later` (outlined, a clock), `known` (a double-ruled stamp),
	`passed` (a plain label), and after a keep `waiting` (dashed, an
	hourglass), `refused` (muted, a no sign) and `missed` (amber, the warning
	sign). It prints its word unless children give another, and takes no
	`tone` or `size`. `Sticker` prints the same words on a sleeve.
-->
<script lang="ts" module>
	import { Ban, Check, Clock, Hourglass, TriangleAlert } from 'lucide-svelte';
	import type { KitIcon } from './Button.svelte';

	export type StateLook = 'kept' | 'later' | 'known' | 'passed' | 'waiting' | 'refused' | 'missed';

	/** Each state's word and mark, shared by the chip and the sticker. */
	export const STATE_LOOKS: Record<StateLook, { word: string; icon?: KitIcon }> = {
		kept: { word: 'Kept', icon: Check },
		later: { word: 'Later', icon: Clock },
		known: { word: 'Know it' },
		passed: { word: 'Passed' },
		waiting: { word: 'Waiting', icon: Hourglass },
		refused: { word: 'Refused', icon: Ban },
		missed: { word: "Didn't arrive", icon: TriangleAlert }
	};
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';

	type Tone = 'action' | 'neutral' | 'info' | 'success' | 'warning' | 'error';

	interface ToneProps {
		tone?: Tone;
		/** `md` stands in a row of buttons, at their height. */
		size?: 'xs' | 'sm' | 'md';
		/** Let a long label wrap inside a narrow card instead of overflowing. */
		wrap?: boolean;
		icon?: KitIcon;
		look?: never;
		/** Layout only: margin, flex. */
		class?: string;
		children: Snippet;
	}

	interface LookProps {
		look: StateLook;
		tone?: never;
		size?: never;
		wrap?: never;
		icon?: never;
		/** Layout only: margin, flex. */
		class?: string;
		/** Another word for the state; the look prints its own without it. */
		children?: Snippet;
	}

	type Props = ToneProps | LookProps;

	let {
		tone = 'neutral',
		size = 'sm',
		wrap = false,
		icon: Icon,
		look,
		class: className = '',
		children
	}: Props = $props();

	const LookIcon = $derived(look ? STATE_LOOKS[look].icon : undefined);

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

{#if look}
	<span class="dn-chip {className}" data-look={look}>
		{#if LookIcon}<LookIcon class="dn-chip__icon" aria-hidden="true" />{/if}
		{#if children}{@render children()}{:else}{STATE_LOOKS[look].word}{/if}
	</span>
{:else}
	<span
		class="inline-flex gap-1 {layout} {sizes[size]} {size === 'md'
			? fills[tone]
			: chips[tone]} {className}"
	>
		{#if Icon}<Icon class="size-3" aria-hidden="true" />{/if}
		{@render children?.()}
	</span>
{/if}

<style>
	/* a pick's state: mono caps in a pill, its colour from the surface it is
	   on, so it reads on the page and on a paper slip alike */
	.dn-chip {
		display: inline-flex;
		flex: none;
		align-items: center;
		gap: 0.25rem;
		height: 1.375rem;
		padding: 0 0.5rem 0 0.375rem;
		border: 1.5px solid transparent;
		border-radius: 9999px;
		font-family: var(--font-mono);
		font-size: 0.65625rem;
		font-weight: 700;
		line-height: 1;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		white-space: nowrap;
	}
	.dn-chip :global(.dn-chip__icon) {
		width: 0.75rem;
		height: 0.75rem;
		flex: none;
		stroke-width: 3;
	}
	.dn-chip[data-look='kept'] {
		border-color: var(--dn-action-edge);
		background-color: var(--dn-action);
		color: var(--dn-action-content);
	}
	.dn-chip[data-look='later'] {
		border-color: currentColor;
	}
	.dn-chip[data-look='known'] {
		padding: 0 0.375rem;
		border: 3px double currentColor;
		border-radius: 3px;
		color: var(--dn-muted);
	}
	.dn-chip[data-look='passed'] {
		padding: 0;
		color: var(--dn-muted);
	}
	.dn-chip[data-look='waiting'] {
		border: 1.5px dashed currentColor;
	}
	.dn-chip[data-look='refused'] {
		border-color: currentColor;
		color: var(--dn-muted);
	}
	.dn-chip[data-look='missed'] {
		border-color: currentColor;
		color: var(--dn-warn-ink, var(--color-warning));
	}
</style>
