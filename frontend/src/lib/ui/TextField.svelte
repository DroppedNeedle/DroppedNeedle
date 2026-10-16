<!--
	A labelled single-line text input.
	Replaces the hand-written `input input-bordered` rows in the settings tabs
	and dialogs.
-->
<script lang="ts">
	import type { FullAutoFill } from 'svelte/elements';
	import Field from './Field.svelte';
	import Input from './Input.svelte';

	interface Props {
		label: string;
		value: string;
		help?: string;
		error?: string | null;
		type?: 'text' | 'url' | 'email' | 'search';
		placeholder?: string;
		/** Monospace, for URLs, paths and ids. */
		mono?: boolean;
		disabled?: boolean;
		required?: boolean;
		autocomplete?: FullAutoFill;
	}

	let {
		label,
		value = $bindable(''),
		help,
		error,
		type = 'text',
		placeholder,
		mono = false,
		disabled = false,
		required = false,
		autocomplete = 'off'
	}: Props = $props();
</script>

<Field {label} {help} {error}>
	{#snippet children(control)}
		<Input
			{...control}
			{label}
			{type}
			{mono}
			error={Boolean(error)}
			class="w-full"
			{placeholder}
			{disabled}
			{required}
			{autocomplete}
			bind:value
		/>
	{/snippet}
</Field>
