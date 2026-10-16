<!--
	A button that shows a spinner and refuses clicks while its work runs. The
	work is either the promise its own click handler returns or a mutation's
	`isPending`, passed as `pending`.
	Replaces the inline `loading loading-spinner` spans and the hand-kept
	`saving`/`removing` flags around buttons.
-->
<script lang="ts">
	import type { ComponentProps, Snippet } from 'svelte';
	import Button from './Button.svelte';

	interface Props extends Omit<
		ComponentProps<typeof Button>,
		'onclick' | 'loading' | 'href' | 'as' | 'children'
	> {
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
		ref = $bindable(null),
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

<Button {...rest} bind:ref loading={busy} onclick={handleClick}>
	{#if busy && pendingLabel}
		{pendingLabel}
	{:else}
		{@render children()}
	{/if}
</Button>
