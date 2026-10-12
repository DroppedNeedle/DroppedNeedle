<!--
	A button that shows a spinner and refuses clicks while its work runs. The
	work is either the promise its own click handler returns or a mutation's
	`isPending`, passed as `pending`.
	Replaces the inline `loading loading-spinner` spans and the hand-kept
	`saving`/`removing` flags around buttons.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLButtonAttributes } from 'svelte/elements';

	interface Props extends Omit<HTMLButtonAttributes, 'onclick'> {
		onclick?: () => unknown;
		pending?: boolean;
		/** Shown in place of the label while pending, e.g. "Saving...". */
		pendingLabel?: string;
		children: Snippet;
	}

	let {
		onclick,
		pending = false,
		pendingLabel,
		disabled = false,
		type = 'button',
		class: className = 'btn btn-primary',
		children,
		...rest
	}: Props = $props();

	let running = $state(false);
	const busy = $derived(pending || running);

	async function handleClick() {
		if (busy || !onclick) return;
		running = true;
		try {
			await onclick();
		} finally {
			running = false;
		}
	}
</script>

<button
	{...rest}
	{type}
	class={className}
	disabled={disabled || busy}
	aria-busy={busy}
	onclick={handleClick}
>
	{#if busy}
		<span class="loading loading-spinner loading-sm" aria-hidden="true"></span>
	{/if}
	{#if busy && pendingLabel}
		{pendingLabel}
	{:else}
		{@render children()}
	{/if}
</button>
