<!--
	A native modal dialog with a heading, a body and an optional action row.
	It owns showModal() and close(), names itself from its title, closes on
	Escape and on a backdrop click, and puts focus back where it was.
	Replaces the hand-built `<dialog class="modal">` blocks, their open effects
	and their focus-restore code.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		open: boolean;
		title: string;
		size?: 'sm' | 'md' | 'lg' | 'xl';
		/** False keeps Escape and the backdrop from closing it, for work in flight. */
		dismissible?: boolean;
		onclose?: () => void;
		children: Snippet;
		actions?: Snippet;
	}

	let {
		open = $bindable(false),
		title,
		size = 'md',
		dismissible = true,
		onclose,
		children,
		actions
	}: Props = $props();

	const titleId = $props.id();
	const widths = { sm: 'max-w-sm', md: 'max-w-md', lg: 'max-w-2xl', xl: 'max-w-4xl' };

	let dialog = $state<HTMLDialogElement>();

	let opener: Element | null = null;

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) {
			opener = document.activeElement;
			dialog.showModal();
		} else if (!open && dialog.open) {
			dialog.close();
		}
	});

	// close() hands focus back by itself. A dialog removed while still open
	// (an `{#if}` around it) never gets that, so do it here.
	$effect(() => {
		const el = dialog;
		return () => {
			if (!el?.open) return;
			el.close();
			if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
		};
	});

	function handleClose() {
		if (!open) return;
		open = false;
		onclose?.();
	}

	function handleCancel(event: Event) {
		if (!dismissible) event.preventDefault();
	}
</script>

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby={titleId}
	onclose={handleClose}
	oncancel={handleCancel}
>
	<div class="modal-box {widths[size]}">
		<h2 id={titleId} class="text-lg font-bold">{title}</h2>
		<div class="pt-4">
			{@render children()}
		</div>
		{#if actions}
			<div class="modal-action">
				{@render actions()}
			</div>
		{/if}
	</div>
	{#if dismissible}
		<form method="dialog" class="modal-backdrop">
			<button tabindex="-1">Close</button>
		</form>
	{/if}
</dialog>
