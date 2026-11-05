<!--
	A button that opens a small panel of controls under it, such as a list's
	filters. The panel closes on a click outside it, on Escape (focus goes
	back to the button) and through the `close` its content is given. The
	button says how many settings in the panel are on, and turns primary
	while any is. The panel stays inside the window on a phone.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ChevronDown } from 'lucide-svelte';
	import Button, { type KitIcon } from './Button.svelte';

	interface Props {
		/** The button's text. */
		label: string;
		/** The panel's name, for screen readers. */
		title: string;
		icon?: KitIcon;
		/** How many settings in the panel are on; shown on the button when above 0. */
		count?: number;
		/** A down arrow after the label, so the button reads as a menu. */
		caret?: boolean;
		/** Which edge of the button the panel lines up with. */
		align?: 'start' | 'end';
		/** Layout only: width, margin, flex. */
		class?: string;
		children: Snippet<[close: () => void]>;
	}

	let {
		label,
		title,
		icon = undefined,
		count = 0,
		caret = false,
		align = 'end',
		class: className = '',
		children
	}: Props = $props();

	/** The panel's width, and the least room it keeps from the window's edges. */
	const WIDTH = 320;
	const GUTTER = 16;

	let open = $state(false);
	let root: HTMLDivElement | undefined = $state();
	let left = $state(0);
	let width = $state(WIDTH);
	const panelId = $props.id();

	function close(): void {
		open = false;
	}

	// Line the panel up with the button, then keep it inside the window: a
	// button near a phone's edge would otherwise push it off screen.
	function place(): void {
		if (!root) return;
		const box = root.getBoundingClientRect();
		const room = document.documentElement.clientWidth;
		width = Math.min(WIDTH, room - 2 * GUTTER);
		const wanted = align === 'end' ? box.width - width : 0;
		const least = GUTTER - box.left;
		const most = room - GUTTER - width - box.left;
		left = Math.max(least, Math.min(wanted, most));
	}

	function toggle(): void {
		if (!open) place();
		open = !open;
	}

	// The panel takes focus when it opens, on its first control.
	function focusFirst(panel: HTMLElement): void {
		const first = panel.querySelector<HTMLElement>(
			'button, input, select, textarea, [href], [tabindex]:not([tabindex="-1"])'
		);
		(first ?? panel).focus();
	}

	// Tabbing out of the button and panel closes it. A click elsewhere is
	// the pointer handler's; focus that lands nowhere (a click on the
	// panel's text) keeps it open.
	function leave(event: FocusEvent): void {
		const next = event.relatedTarget;
		if (next instanceof Node && root && !root.contains(next)) open = false;
	}

	// The count's badge changes the button's width; line up again.
	$effect(() => {
		if (open && count >= 0) place();
	});

	$effect(() => {
		if (!open) return;
		const away = (event: PointerEvent) => {
			if (root && !root.contains(event.target as Node)) open = false;
		};
		const escape = (event: KeyboardEvent) => {
			if (event.key !== 'Escape') return;
			open = false;
			root?.querySelector<HTMLButtonElement>('button')?.focus();
		};
		document.addEventListener('pointerdown', away);
		document.addEventListener('keydown', escape);
		window.addEventListener('resize', place);
		return () => {
			document.removeEventListener('pointerdown', away);
			document.removeEventListener('keydown', escape);
			window.removeEventListener('resize', place);
		};
	});
</script>

<div class="relative {className}" bind:this={root} onfocusout={leave}>
	<Button
		variant={count > 0 ? 'primary' : 'outline'}
		{icon}
		class={caret ? 'w-full justify-start' : ''}
		aria-expanded={open}
		aria-haspopup="dialog"
		aria-controls={open ? panelId : undefined}
		onclick={toggle}
	>
		{label}{#if count > 0}<span class="badge badge-sm ml-1">{count}</span
			>{/if}{#if caret}<ChevronDown class="ml-auto size-4" aria-hidden="true" />{/if}
	</Button>
	{#if open}
		<div
			id={panelId}
			class="absolute top-full z-30 mt-2 space-y-4 rounded-box border border-line bg-base-200 p-4 shadow-xl"
			style:left="{left}px"
			style:width="{width}px"
			role="dialog"
			aria-label={title}
			tabindex="-1"
			use:focusFirst
		>
			{@render children(close)}
		</div>
	{/if}
</div>
