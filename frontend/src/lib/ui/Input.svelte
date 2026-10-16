<!--
	A bare single-line input: search boxes, inline edits, filters. `label` is
	its accessible name. A labelled form row uses TextField, NumberField or
	SecretField instead, which frame this with a visible label.
	Replaces the hand-written `input`, `input-sm` and `input-soft` inputs.
-->
<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';
	import type { KitIcon } from './Button.svelte';

	interface Props extends Omit<HTMLInputAttributes, 'type' | 'size' | 'value' | 'children'> {
		label: string;
		value?: string | number | null;
		type?: 'text' | 'search' | 'url' | 'email' | 'password' | 'number' | 'date' | 'time';
		size?: 'sm' | 'md';
		/** Monospace, for URLs, paths and ids. */
		mono?: boolean;
		error?: boolean;
		/** Shown inside the start of the box. */
		icon?: KitIcon;
		ref?: HTMLInputElement | null;
	}

	let {
		label,
		value = $bindable(),
		type = 'text',
		size = 'md',
		mono = false,
		error = false,
		icon: Icon,
		ref = $bindable(null),
		class: className = '',
		...rest
	}: Props = $props();

	const sizes = { sm: 'input input-sm', md: 'input' };
	const box = $derived(
		[sizes[size], mono ? 'font-mono text-sm' : '', error ? 'input-error' : '', className]
			.filter(Boolean)
			.join(' ')
	);
</script>

{#if Icon}
	<label class={box}>
		<Icon class="size-4 shrink-0 opacity-60" aria-hidden="true" />
		<input
			{...rest}
			bind:this={ref}
			{type}
			class="grow"
			aria-label={label}
			aria-invalid={error || rest['aria-invalid']}
			bind:value
		/>
	</label>
{:else}
	<input
		{...rest}
		bind:this={ref}
		{type}
		class={box}
		aria-label={label}
		aria-invalid={error || rest['aria-invalid']}
		bind:value
	/>
{/if}
