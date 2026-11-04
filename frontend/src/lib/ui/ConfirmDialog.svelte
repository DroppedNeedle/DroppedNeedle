<!--
	Asks before doing something, on top of Modal. Confirming runs `onconfirm`:
	the dialog stays open and busy until that settles, closes when it succeeds
	and shows the error when it fails. `phrase` makes the user type a word
	first. `icon` sets a mark beside the title, tinted by `tone`, and
	`focusTitle` puts focus on the title rather than the first control.
	Replaces `window.confirm`, the one-off confirm modals such as
	DeleteAlbumModal, and the typed-phrase confirmations in library management.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import AsyncButton from './AsyncButton.svelte';
	import Button, { type KitIcon } from './Button.svelte';
	import Input from './Input.svelte';
	import Modal from './Modal.svelte';

	interface Props {
		open: boolean;
		title: string;
		/** Plain text. Use `children` when the message needs markup or extra controls. */
		message?: string;
		confirmLabel?: string;
		cancelLabel?: string;
		tone?: 'primary' | 'warning' | 'danger';
		icon?: KitIcon;
		focusTitle?: boolean;
		/** The user must type this exactly before Confirm unlocks. */
		phrase?: string;
		/** A mutation's `isPending`, when the caller runs the work itself. */
		pending?: boolean;
		/** False when there turns out to be nothing to confirm: only the cancel button shows. */
		canConfirm?: boolean;
		onconfirm: () => unknown;
		oncancel?: () => void;
		children?: Snippet;
	}

	let {
		open = $bindable(false),
		title,
		message,
		confirmLabel = 'Confirm',
		cancelLabel = 'Cancel',
		tone = 'primary',
		icon,
		focusTitle = false,
		phrase,
		pending = false,
		canConfirm = true,
		onconfirm,
		oncancel,
		children
	}: Props = $props();

	const phraseId = $props.id();

	let running = $state(false);
	let confirmed = false;
	let error = $state<string | null>(null);
	let typed = $state('');
	const busy = $derived(pending || running);
	const iconTone = $derived(
		tone === 'danger' ? 'error' : tone === 'warning' ? 'warning' : 'accent'
	);

	$effect(() => {
		if (open) {
			confirmed = false;
			error = null;
			typed = '';
		}
	});

	async function confirm() {
		running = true;
		error = null;
		try {
			await onconfirm();
			confirmed = true;
		} catch (e) {
			error = e instanceof Error && e.message ? e.message : 'That did not work. Try again.';
		} finally {
			running = false;
		}
		if (confirmed) open = false;
	}

	function handleClose() {
		if (!confirmed) oncancel?.();
	}
</script>

<Modal bind:open {title} {icon} {iconTone} {focusTitle} dismissible={!busy} onclose={handleClose}>
	{#if message}
		<p class="text-base-content/70">{message}</p>
	{/if}
	{@render children?.()}

	{#if phrase}
		<label class="mt-4 block text-sm" for={phraseId}>
			Type <strong>{phrase}</strong> to continue
		</label>
		<Input
			id={phraseId}
			label="Type {phrase} to continue"
			class="mt-1 w-full"
			autocomplete="off"
			spellcheck="false"
			bind:value={typed}
			disabled={busy}
		/>
	{/if}

	{#if error}
		<div class="alert alert-error mt-3 text-sm" role="alert">
			<span>{error}</span>
		</div>
	{/if}

	{#snippet actions()}
		<Button variant="ghost" onclick={() => (open = false)} disabled={busy}>
			{cancelLabel}
		</Button>
		{#if canConfirm}
			<AsyncButton
				variant={tone}
				onclick={confirm}
				pending={busy}
				disabled={phrase !== undefined && typed !== phrase}
			>
				{confirmLabel}
			</AsyncButton>
		{/if}
	{/snippet}
</Modal>
