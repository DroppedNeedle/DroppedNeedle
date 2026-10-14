<!--
	A labelled list of short text values, one per row, with a button to add a
	row and one on each row to remove it.
	For settings that hold a few addresses, paths or names.
-->
<script lang="ts">
	import { tick, type Snippet } from 'svelte';
	import { Plus, X } from 'lucide-svelte';

	interface Props {
		label: string;
		values: string[];
		/** What one row holds, for the row labels: "Address". */
		itemLabel: string;
		addLabel?: string;
		/** Add is disabled once the list holds this many rows. */
		max?: number;
		help?: string;
		error?: string | null;
		placeholder?: string;
		/** Monospace, for URLs, paths and ids. */
		mono?: boolean;
		disabled?: boolean;
		/** Shown while the list has no rows. */
		empty?: string;
		/** More buttons beside Add. */
		actions?: Snippet;
	}

	let {
		label,
		values = $bindable([]),
		itemLabel,
		addLabel = 'Add',
		max,
		help,
		error,
		placeholder,
		mono = false,
		disabled = false,
		empty,
		actions
	}: Props = $props();

	const id = $props.id();
	const helpId = `${id}-help`;
	const errorId = `${id}-error`;

	const describedBy = $derived(
		[error ? errorId : null, help ? helpId : null].filter(Boolean).join(' ') || undefined
	);
	const full = $derived(max !== undefined && values.length >= max);

	let rows = $state<(HTMLInputElement | undefined)[]>([]);
	let addButton = $state<HTMLButtonElement>();

	// Keyboard focus follows the change: into the new row, or onto the row
	// that took the removed one's place (the Add button when none is left).
	async function add() {
		values = [...values, ''];
		await tick();
		rows[values.length - 1]?.focus();
	}

	async function remove(index: number) {
		values = values.filter((_, at) => at !== index);
		await tick();
		(rows[Math.min(index, values.length - 1)] ?? addButton)?.focus();
	}
</script>

<fieldset class="fieldset">
	<legend class="fieldset-legend">{label}</legend>
	{#if values.length === 0 && empty}
		<p class="text-sm text-base-content/60">{empty}</p>
	{/if}
	{#each values as value, index (index)}
		<div class="flex gap-2">
			<input
				type="text"
				class="input w-full {mono ? 'font-mono text-sm' : ''} {error ? 'input-error' : ''}"
				aria-label="{itemLabel} {index + 1}"
				aria-invalid={Boolean(error)}
				aria-describedby={describedBy}
				{placeholder}
				{disabled}
				autocomplete="off"
				spellcheck="false"
				bind:this={rows[index]}
				bind:value={values[index]}
			/>
			<button
				type="button"
				class="btn btn-ghost btn-square shrink-0"
				aria-label="Remove {value.trim() || `${itemLabel.toLowerCase()} ${index + 1}`}"
				{disabled}
				onclick={() => remove(index)}
			>
				<X class="size-4" aria-hidden="true" />
			</button>
		</div>
	{/each}
	<div class="flex flex-wrap gap-2">
		<button
			type="button"
			class="btn btn-sm"
			disabled={disabled || full}
			bind:this={addButton}
			onclick={add}
		>
			<Plus class="size-4" aria-hidden="true" />
			{addLabel}
		</button>
		{#if actions}
			{@render actions()}
		{/if}
	</div>
	{#if help}
		<p id={helpId} class="label whitespace-normal text-base-content/60">{help}</p>
	{/if}
	{#if error}
		<p id={errorId} class="text-sm text-error" role="alert">{error}</p>
	{/if}
</fieldset>
