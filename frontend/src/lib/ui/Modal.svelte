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
	`icon` sets a tinted mark beside the title, in `iconTone` (`action` is the
	acid tile with dark ink); a sheet shows it too.
	`bottomSheet` rises from the bottom edge on a phone and sits centred from
	`sm` up.
	`focusTitle` puts focus on the title when it opens, for a dialog that is
	read before it is acted on; otherwise focus goes to its first control.
	`onrequestclose` makes Escape, the backdrop and the close button ask the
	caller instead of closing, for a form that confirms before it drops unsaved
	changes. The caller closes it by setting `open` to false.
	`overflowVisible` lets a dropdown inside reach past the box's edge, for a
	short dialog that holds one.
	`paper` prints a non-sheet dialog on paper stock (the sheet's theme inside,
	an Anton title), for a confirm that starts a write to files.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { X } from 'lucide-svelte';
	import type { KitIcon } from './Button.svelte';
	import IconButton from './IconButton.svelte';
	import { themeStore } from '$lib/stores/theme.svelte';

	interface Props {
		open: boolean;
		title: string;
		/** Makes the title a link. */
		titleHref?: string;
		/** One line under the title; the dialog's accessible description. */
		description?: string;
		size?: 'sm' | 'md' | 'lg' | 'xl' | 'sheet';
		/** A lucide icon shown in a tinted mark beside the title. */
		icon?: KitIcon;
		iconTone?: 'accent' | 'action' | 'warning' | 'error';
		/** On a phone, rise from the bottom edge. */
		bottomSheet?: boolean;
		focusTitle?: boolean;
		/** False keeps Escape and the backdrop from closing it, for work in flight. */
		dismissible?: boolean;
		/** A close button beside the title; `sheet` always has one. */
		closeButton?: boolean;
		/** Called in place of closing on Escape, the backdrop or the close button. */
		onrequestclose?: () => void;
		/** Lets a dropdown inside reach past the box's edge. */
		overflowVisible?: boolean;
		/** Paper stock with an Anton title, for a confirm that starts a write. */
		paper?: boolean;
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
		icon: Icon,
		iconTone = 'accent',
		bottomSheet = false,
		focusTitle = false,
		dismissible = true,
		closeButton = false,
		onrequestclose,
		overflowVisible = false,
		paper = false,
		onclose,
		children,
		actions
	}: Props = $props();

	const titleId = $props.id();
	const descriptionId = `${titleId}-description`;
	const widths = { sm: 'max-w-sm', md: 'max-w-md', lg: 'max-w-2xl', xl: 'max-w-4xl' };
	const sheet = $derived(size === 'sheet');
	const theme = $derived(themeStore.resolved);
	const iconTones = {
		accent: 'bg-accent/10 text-accent',
		action: 'border border-action-edge bg-action text-action-content',
		warning: 'bg-warning/12 text-warning',
		error: 'bg-error/12 text-error'
	};

	let dialog = $state<HTMLDialogElement>();

	let opener: Element | null = null;

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) {
			opener = document.activeElement;
			dialog.showModal();
			if (focusTitle) document.getElementById(titleId)?.focus();
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
		if (!dismissible || onrequestclose) event.preventDefault();
		if (dismissible) onrequestclose?.();
	}

	function requestClose() {
		if (onrequestclose) onrequestclose();
		else dialog?.close();
	}

	function handleBackdrop(event: Event) {
		if (!onrequestclose) return;
		event.preventDefault();
		onrequestclose();
	}
</script>

{#snippet heading(className: string)}
	<!-- The title is focusable only for focusTitle, and only by script (-1). -->
	<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
	<h2
		id={titleId}
		class={[className, focusTitle && 'focus:outline-none']}
		tabindex={focusTitle ? -1 : undefined}
	>
		{#if titleHref}
			<a
				href={titleHref}
				class="rounded-sm transition-colors hover:text-accent"
				onclick={() => dialog?.close()}>{title}</a
			>
		{:else}
			{title}
		{/if}
	</h2>
{/snippet}

<dialog
	bind:this={dialog}
	class={['modal', bottomSheet && 'modal-bottom sm:modal-middle']}
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
				class="sticky top-0 z-10 flex items-start justify-between gap-3 border-b border-line bg-base-200 px-4 py-3 md:static md:px-6 md:py-4"
			>
				{#if Icon}
					<div
						class="grid h-10 w-10 shrink-0 place-items-center rounded-xl {iconTones[iconTone]}"
						aria-hidden="true"
					>
						<Icon class="h-5 w-5" />
					</div>
				{/if}
				<div class={['min-w-0 flex-1', Icon && 'self-center']}>
					{@render heading('text-xl leading-tight')}
					{#if description}
						<p id={descriptionId} class="mt-0.5 text-sm text-muted">{description}</p>
					{/if}
				</div>
				{#if dismissible}
					<IconButton
						shape="square"
						size="sm"
						icon={X}
						label="Close"
						onclick={() => requestClose()}
					/>
				{/if}
			</header>
			<div class="flex-1 md:min-h-0">
				{@render children()}
			</div>
			{#if actions}
				<div
					class="sticky bottom-0 z-10 border-t border-line bg-base-200 px-4 pt-3 pb-[calc(env(safe-area-inset-bottom)+0.75rem)] md:static md:px-6 md:pb-3"
				>
					{@render actions()}
				</div>
			{/if}
		</div>
	{:else}
		<div
			class={[
				'modal-box',
				widths[size as keyof typeof widths],
				overflowVisible && 'overflow-visible',
				paper && 'border-2 border-paper-content',
				bottomSheet && 'max-sm:max-w-none pb-[calc(env(safe-area-inset-bottom)+1.5rem)] sm:pb-6'
			]}
			data-theme={paper ? (theme === 'midnight' ? 'midnight' : 'light') : undefined}
		>
			<div class="flex items-start gap-3">
				{#if Icon}
					<div
						class="grid h-11 w-11 shrink-0 place-items-center rounded-xl {iconTones[iconTone]}"
						aria-hidden="true"
					>
						<Icon class="h-5 w-5" />
					</div>
				{/if}
				<div class={['min-w-0 flex-1', Icon && 'self-center']}>
					{@render heading(
						paper
							? 'font-display text-3xl leading-none font-normal uppercase'
							: 'text-xl leading-tight'
					)}
					{#if description}
						<p id={descriptionId} class="mt-1 text-sm text-muted">{description}</p>
					{/if}
				</div>
				{#if closeButton && dismissible}
					<IconButton
						shape="square"
						size="sm"
						icon={X}
						label="Close"
						class="-mt-1 -mr-2 shrink-0"
						onclick={() => requestClose()}
					/>
				{/if}
			</div>
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
			<button tabindex="-1" onclick={handleBackdrop}>Close</button>
		</form>
	{/if}
</dialog>
