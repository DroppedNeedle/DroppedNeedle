<!--
	A native modal dialog with a heading, a body and an optional action row.
	It owns showModal() and close(), names itself from its title, closes on
	Escape and on a backdrop click, and puts focus back where it was.
	Replaces the hand-built `<dialog class="modal">` blocks, their open effects
	and their focus-restore code.

	`size="sheet"` is for a screen-sized task: full height with a sticky header
	and action bar on a phone, a large centred box from `md` up whose body
	lays out its own scroll panes. It has a close button in its header.
	`closeButton` adds the same header close to the other sizes, for a dialog
	with no Cancel of its own.
	`titleHref` makes the title a link to the thing the dialog shows; it still
	names the dialog, and following it closes the dialog.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { X } from 'lucide-svelte';

	interface Props {
		open: boolean;
		title: string;
		/** Makes the title a link. */
		titleHref?: string;
		/** One line under the title; the dialog's accessible description. */
		description?: string;
		size?: 'sm' | 'md' | 'lg' | 'xl' | 'sheet';
		/** False keeps Escape and the backdrop from closing it, for work in flight. */
		dismissible?: boolean;
		/** A close button beside the title; `sheet` always has one. */
		closeButton?: boolean;
		onclose?: () => void;
		children: Snippet;
		actions?: Snippet;
	}

	let {
		open = $bindable(false),
		title,
		titleHref,
		description,
		size = 'md',
		dismissible = true,
		closeButton = false,
		onclose,
		children,
		actions
	}: Props = $props();

	const titleId = $props.id();
	const descriptionId = `${titleId}-description`;
	const widths = { sm: 'max-w-sm', md: 'max-w-md', lg: 'max-w-2xl', xl: 'max-w-4xl' };
	const sheet = $derived(size === 'sheet');

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

{#snippet heading(className: string)}
	<h2 id={titleId} class={className}>
		{#if titleHref}
			<a
				href={titleHref}
				class="rounded-sm transition-colors hover:text-accent focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
				onclick={() => dialog?.close()}>{title}</a
			>
		{:else}
			{title}
		{/if}
	</h2>
{/snippet}

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby={titleId}
	aria-describedby={description ? descriptionId : undefined}
	onclose={handleClose}
	oncancel={handleCancel}
>
	{#if sheet}
		<div
			class="modal-box flex h-dvh max-h-dvh w-full max-w-none flex-col overflow-y-auto rounded-none p-0 md:h-[min(88dvh,52rem)] md:max-h-[min(88dvh,52rem)] md:max-w-6xl md:overflow-hidden md:rounded-box"
			data-testid="modal-sheet"
		>
			<header
				class="sticky top-0 z-10 flex items-start justify-between gap-3 border-b border-base-content/10 bg-base-100 px-4 py-3 md:static md:px-6 md:py-4"
			>
				<div class="min-w-0">
					{@render heading('text-lg font-bold')}
					{#if description}
						<p id={descriptionId} class="mt-0.5 text-sm text-base-content/65">{description}</p>
					{/if}
				</div>
				{#if dismissible}
					<button
						type="button"
						class="btn btn-square btn-ghost btn-sm"
						aria-label="Close"
						onclick={() => dialog?.close()}><X class="h-4 w-4" /></button
					>
				{/if}
			</header>
			<div class="flex-1 md:min-h-0">
				{@render children()}
			</div>
			{#if actions}
				<div
					class="sticky bottom-0 z-10 border-t border-base-content/10 bg-base-100 px-4 pt-3 pb-[calc(env(safe-area-inset-bottom)+0.75rem)] md:static md:px-6 md:pb-3"
				>
					{@render actions()}
				</div>
			{/if}
		</div>
	{:else}
		<div class="modal-box {widths[size as keyof typeof widths]}">
			{#if closeButton && dismissible}
				<div class="flex items-start justify-between gap-3">
					{@render heading('min-w-0 text-lg font-bold')}
					<button
						type="button"
						class="btn btn-square btn-ghost btn-sm -mt-1 -mr-2 shrink-0"
						aria-label="Close"
						onclick={() => dialog?.close()}><X class="h-4 w-4" /></button
					>
				</div>
			{:else}
				{@render heading('text-lg font-bold')}
			{/if}
			{#if description}
				<p id={descriptionId} class="mt-1 text-sm text-base-content/65">{description}</p>
			{/if}
			<div class="pt-4">
				{@render children()}
			</div>
			{#if actions}
				<div class="modal-action">
					{@render actions()}
				</div>
			{/if}
		</div>
	{/if}
	{#if dismissible}
		<form method="dialog" class="modal-backdrop">
			<button tabindex="-1">Close</button>
		</form>
	{/if}
</dialog>
