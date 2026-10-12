<!--
	A labelled number input with an optional unit.
	Replaces `settings/SettingsNumberField.svelte` and the hand-written number
	inputs beside it.
-->
<script lang="ts">
	import Field from './Field.svelte';

	interface Props {
		label: string;
		value: number;
		help?: string;
		error?: string | null;
		min?: number;
		max?: number;
		step?: number;
		/** Shown after the number, e.g. "hrs". */
		unit?: string;
		disabled?: boolean;
	}

	let {
		label,
		value = $bindable(0),
		help,
		error,
		min,
		max,
		step = 1,
		unit,
		disabled = false
	}: Props = $props();
</script>

<Field {label} {help} {error}>
	{#snippet children(control)}
		<div class="input w-full {error ? 'input-error' : ''}">
			<input {...control} type="number" class="grow" {min} {max} {step} {disabled} bind:value />
			{#if unit}
				<span class="label">{unit}</span>
			{/if}
		</div>
	{/snippet}
</Field>
