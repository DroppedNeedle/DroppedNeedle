<!--
	A button that opens a small panel of controls under it, such as a list's
	filters. The panel closes on a click outside it, on Escape (focus goes
	back to the button) and through the `close` its content is given. The
	button says how many settings in the panel are on, and turns primary
	while any is. The panel stays inside the window on a phone.
	`look="icon"` makes the button an icon alone (`label` is its name), with
	an optional short mono `tag` beside it; `on` fills it. `side="above"`
	opens the panel over the button, for one along the bottom of the window.
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
		/** Where the panel opens. */
		side?: 'below' | 'above';
		/** An icon button in place of the labelled one. */
		look?: 'button' | 'icon';
		/** A short mono mark beside the icon, in `look="icon"`; `label` stays the name. */
		tag?: Snippet;
		/** Fills the button; by default, while `count` is above 0. */
		on?: boolean;
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
		side = 'below',
		look = 'button',
		tag = undefined,
		on = undefined,
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
	const filled = $derived(on ?? count > 0);

	// Shut the panel with focus back on the button, as Escape does.
	function close(): void {
		open = false;
		root?.querySelector<HTMLButtonElement>('button')?.focus();
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
			if (event.key === 'Escape') close();
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
	{#if look === 'icon'}
		<Button
			variant={filled ? 'primary' : 'ghost'}
			{icon}
			pill
			class="[&>svg]:size-5 {tag ? '' : 'btn-circle'}"
			aria-label={label}
			title={label}
			aria-expanded={open}
			aria-haspopup="dialog"
			aria-controls={open ? panelId : undefined}
			onclick={toggle}
		>
			{#if tag}<span class="font-mono text-xs font-bold">{@render tag()}</span>{/if}
		</Button>
	{:else}
		<Button
			variant={filled ? 'primary' : 'outline'}
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
	{/if}
	{#if open}
		<div
			id={panelId}
			class="absolute z-30 space-y-4 rounded-box border border-line bg-base-200 p-4 shadow-xl {side ===
			'above'
				? 'bottom-full mb-2'
				: 'top-full mt-2'}"
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
