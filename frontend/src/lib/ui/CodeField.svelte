<!--
	A labelled CSS editor: line numbers, syntax colours, Tab to indent,
	completion of the `words` passed in, and a marker on each line `problems`
	refuses, checked again whenever `problems` changes. Escape then Tab leaves
	the field, so the keyboard is never trapped. CodeMirror is loaded only
	when the field first shows. `limit` adds a count of the characters used.
	Call `insert(text)` (through `bind:this`) to put text at the cursor.
	`aside` sits at the end of the label's row.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { CodeEditor, CodeProblem, CodeWord } from './codeEditor';

	interface Props {
		label: string;
		value: string;
		help?: string;
		error?: string | null;
		placeholder?: string;
		readonly?: boolean;
		/** Shows "used / limit" under the editor. */
		limit?: number;
		/** Offered while typing. */
		words?: CodeWord[];
		/** The lines to mark, with why. */
		problems?: (text: string) => CodeProblem[];
		aside?: Snippet;
	}

	let {
		label,
		value = $bindable(''),
		help,
		error,
		placeholder,
		readonly = false,
		limit,
		words = [],
		problems = () => [],
		aside
	}: Props = $props();

	const id = $props.id();
	let host = $state<HTMLDivElement | null>(null);
	let editor = $state<CodeEditor | null>(null);
	let failed = $state(false);
	let focused = $state(false);

	$effect(() => {
		const target = host;
		if (!target) return;
		let gone = false;
		let made: CodeEditor | null = null;
		import('./codeEditor')
			.then(({ createCodeEditor }) => {
				if (gone) return;
				made = createCodeEditor({
					host: target,
					text: value,
					label,
					readonly,
					placeholder,
					words: () => words,
					problems: (text) => problems(text),
					onchange: (text) => (value = text),
					onfocus: (now) => (focused = now)
				});
				editor = made;
			})
			.catch(() => {
				if (!gone) failed = true;
			});
		return () => {
			gone = true;
			made?.destroy();
			editor = null;
		};
	});

	$effect(() => {
		editor?.setText(value);
	});

	$effect(() => {
		editor?.setReadonly(readonly);
	});

	// A new `problems` may mark other lines in the same text, such as a refused save's.
	$effect(() => {
		void problems;
		editor?.recheck();
	});

	/** Put `text` at the cursor, or in place of the selection. */
	export function insert(text: string): void {
		editor?.insert(text);
	}

	/** Move the keyboard into the editor. */
	export function focus(): void {
		editor?.focus();
	}
</script>

<div class="fieldset code-field" role="group" aria-labelledby="{id}-label">
	<div class="flex flex-wrap items-baseline justify-between gap-x-3">
		<!-- The editor sits in its own shadow root, so a click on the name focuses it by hand. -->
		<button
			type="button"
			id="{id}-label"
			class="fieldset-legend cursor-text"
			tabindex="-1"
			onclick={() => editor?.focus()}>{label}</button
		>
		{#if aside}{@render aside()}{/if}
	</div>
	<div class="code-host" class:focused bind:this={host}>
		{#if failed}
			<p class="p-4 text-sm text-error">The editor did not load. Reload the page to try again.</p>
		{:else if !editor}
			<div class="skeleton h-64 w-full" aria-hidden="true"></div>
		{/if}
	</div>
	<div class="flex flex-wrap items-baseline justify-between gap-x-3">
		{#if error}
			<p class="text-sm text-error" role="alert">{error}</p>
		{/if}
		{#if limit !== undefined}
			<p
				class="ml-auto font-mono text-xs tabular-nums {value.length > limit
					? 'text-error'
					: 'text-muted'}"
			>
				{value.length.toLocaleString()} / {limit.toLocaleString()}
			</p>
		{/if}
	</div>
	{#if help}
		<p class="label whitespace-normal text-muted">{help}</p>
	{/if}
	<p class="sr-only" aria-live="polite">
		{focused ? 'Press Escape then Tab to leave the editor.' : ''}
	</p>
</div>

<style>
	.code-field {
		min-width: 0;
		grid-template-columns: minmax(0, 1fr);
	}
	.code-host {
		min-width: 0;
		min-height: 16rem;
	}
	.fieldset-legend {
		background: none;
		border: 0;
		padding: 0;
		text-align: start;
	}
</style>
