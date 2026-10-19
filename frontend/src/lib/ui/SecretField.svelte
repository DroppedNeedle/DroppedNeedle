<!--
	A labelled input for an API key, token or password, with a show and hide
	button. The server sends a mask in place of a stored secret and reads that
	same mask back as "keep what you have". While the value is the mask the
	field says a secret is saved, and the first edit starts from empty so a
	half-edited mask is never saved as the new secret. Emptying the field
	clears the secret, as it does today.
	Replaces the `showKey`/`showPassword` blocks and the per-tab mask handling.
-->
<script lang="ts">
	import { Eye, EyeOff } from 'lucide-svelte';
	import type { FullAutoFill } from 'svelte/elements';
	import Field from './Field.svelte';

	interface Props {
		label: string;
		value: string;
		/** The server's placeholder for a stored secret (`SECRET_MASK`). */
		mask?: string;
		help?: string;
		error?: string | null;
		placeholder?: string;
		disabled?: boolean;
		autocomplete?: FullAutoFill;
	}

	let {
		label,
		value = $bindable(''),
		mask,
		help,
		error,
		placeholder,
		disabled = false,
		autocomplete = 'off'
	}: Props = $props();

	let revealed = $state(false);
	const stored = $derived(mask !== undefined && value === mask);
	const shown = $derived(revealed && !stored);

	function startFromEmpty(event: InputEvent & { currentTarget: HTMLInputElement }) {
		if (!stored) return;
		event.currentTarget.value = '';
		value = '';
	}
</script>

<Field {label} help={stored ? 'A value is saved. Leave this as it is to keep it.' : help} {error}>
	{#snippet children(control)}
		<div class="join w-full">
			<input
				{...control}
				type={shown ? 'text' : 'password'}
				class="input join-item w-full font-mono text-sm {error ? 'input-error' : ''}"
				{placeholder}
				{disabled}
				{autocomplete}
				spellcheck="false"
				onbeforeinput={startFromEmpty}
				bind:value
			/>
			<button
				type="button"
				class="btn join-item"
				aria-label={shown ? `Hide ${label}` : `Show ${label}`}
				aria-pressed={shown}
				disabled={disabled || stored}
				onclick={() => (revealed = !revealed)}
			>
				{#if shown}
					<EyeOff class="size-4" aria-hidden="true" />
				{:else}
					<Eye class="size-4" aria-hidden="true" />
				{/if}
			</button>
		</div>
	{/snippet}
</Field>
