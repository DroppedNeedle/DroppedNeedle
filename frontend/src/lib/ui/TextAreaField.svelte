<!--
	A labelled multi-line text box that grows with its text, for values a
	person should read whole: scripts, lists, notes.
	For a list kept one value per line, pass `value` without binding and
	parse the text in `oninput`: the box keeps what was typed (a trailing
	blank line included) while the list holds only the parsed values.
	Bind `element` to place text at the cursor; `aside` sits at the end of
	the label's row.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import Field from './Field.svelte';

	interface Props {
		label: string;
		value: string;
		help?: string;
		error?: string | null;
		placeholder?: string;
		/** Monospace, for scripts and paths. */
		mono?: boolean;
		/** Rows shown when the text is shorter. */
		rows?: number;
		disabled?: boolean;
		readonly?: boolean;
		/** Called with the text after each edit. */
		oninput?: (text: string) => void;
		/** The textarea, for placing text at the cursor. */
		element?: HTMLTextAreaElement | null;
		aside?: Snippet;
	}

	let {
		label,
		value = $bindable(''),
		help,
		error,
		placeholder,
		mono = false,
		rows = 2,
		disabled = false,
		readonly = false,
		oninput,
		element = $bindable(null),
		aside
	}: Props = $props();
</script>

<Field {label} {help} {error} {aside}>
	{#snippet children(control)}
		<textarea
			{...control}
			class={['textarea grows w-full', mono && 'font-mono text-sm', error && 'textarea-error']}
			spellcheck={mono ? false : undefined}
			{rows}
			{placeholder}
			{disabled}
			{readonly}
			oninput={(event) => oninput?.(event.currentTarget.value)}
			bind:this={element}
			bind:value></textarea>
	{/snippet}
</Field>

<style>
	.grows {
		field-sizing: content;
		resize: vertical;
	}
</style>
