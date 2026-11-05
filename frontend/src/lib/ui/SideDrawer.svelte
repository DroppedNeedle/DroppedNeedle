<!--
	A panel that slides in from the right over a dimmed page, full height
	down to `bottom` (a bar it must not cover). It floats, inset and
	bordered like the player bar. Opening moves focus to its first control;
	Escape, the page behind it or `onclose` shut it, and focus goes back to
	where it was when the drawer had taken it. Keys pressed inside stay
	inside, so the page's own shortcuts (Space plays, arrows seek) don't
	fire while a control in the drawer has focus. The page doesn't scroll
	behind it.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { fade, fly } from './motion';

	interface Props {
		open: boolean;
		/** Names the drawer for screen readers. */
		label: string;
		/** Names the dimmed page, which closes the drawer when pressed. */
		closeLabel: string;
		onclose: () => void;
		/** The drawer's bottom edge, a CSS length; it stays clear of a bar below. */
		bottom?: string;
		children: Snippet;
	}

	let {
		open = $bindable(),
		label,
		closeLabel,
		onclose,
		bottom = '0.75rem',
		children
	}: Props = $props();

	let panel = $state<HTMLElement | null>(null);
	let scrim = $state<HTMLElement | null>(null);
	let returnTo: HTMLElement | null = null;

	const FOCUSABLE =
		'button:not([disabled]):not([tabindex="-1"]), a[href], input:not([disabled]), select:not([disabled]), [tabindex="0"]';
	const PAGE_KEYS = new Set([' ', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight']);

	function close(): void {
		open = false;
		onclose();
	}

	function onWindowKeydown(event: KeyboardEvent): void {
		if (open && event.key === 'Escape') close();
	}

	function keepKeys(event: KeyboardEvent): void {
		if (PAGE_KEYS.has(event.key)) event.stopPropagation();
	}

	$effect(() => {
		if (!open || !panel) return;
		returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		(panel.querySelector<HTMLElement>(FOCUSABLE) ?? panel).focus();
		document.body.classList.add('overflow-hidden');
		const node = panel;
		return () => {
			document.body.classList.remove('overflow-hidden');
			// Only when the drawer held focus: a bar button that closed it keeps it.
			const active = document.activeElement;
			if (!active || active === document.body || active === scrim || node.contains(active)) {
				if (returnTo?.isConnected) returnTo.focus();
			}
			returnTo = null;
		};
	});
</script>

<svelte:window onkeydown={onWindowKeydown} />

{#if open}
	<button
		bind:this={scrim}
		type="button"
		class="dn-side-drawer__scrim"
		aria-label={closeLabel}
		tabindex="-1"
		onclick={close}
		transition:fade={{ duration: 180 }}
	></button>
	<div
		bind:this={panel}
		class="dn-side-drawer"
		style:bottom
		role="dialog"
		aria-label={label}
		tabindex="-1"
		onkeydown={keepKeys}
		transition:fly={{ x: 420, duration: 220 }}
	>
		{@render children()}
	</div>
{/if}

<style>
	.dn-side-drawer__scrim {
		position: fixed;
		inset: 0;
		z-index: 55;
		background: rgb(0 0 0 / 0.45);
		cursor: default;
	}
	.dn-side-drawer {
		position: fixed;
		top: 0.75rem;
		right: 0.75rem;
		z-index: 55;
		display: flex;
		width: min(28rem, calc(100vw - 1.5rem));
		flex-direction: column;
		overflow: hidden;
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-box);
		background: var(--color-base-200);
		box-shadow: 0 24px 60px -24px rgb(0 0 0 / 0.6);
		outline: none;
	}
</style>
