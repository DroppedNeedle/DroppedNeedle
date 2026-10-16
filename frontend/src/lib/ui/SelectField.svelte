<!--
	A labelled dropdown over a list of options.
	Replaces the hand-written `select select-bordered` blocks.
-->
<script lang="ts" generics="T extends string">
	import Field from './Field.svelte';

	interface Option {
		value: T;
		label: string;
		disabled?: boolean;
	}

	interface Props {
		label: string;
		value: T;
		options: Option[];
		help?: string;
		error?: string | null;
		disabled?: boolean;
		ref?: HTMLSelectElement | null;
	}

	let {
		label,
		value = $bindable(),
		options,
		help,
		error,
		disabled = false,
		ref = $bindable(null)
	}: Props = $props();
</script>

<Field {label} {help} {error}>
	{#snippet children(control)}
		<select
			{...control}
			class="select w-full {error ? 'select-error' : ''}"
			{disabled}
			bind:value
			bind:this={ref}
		>
			{#each options as option (option.value)}
				<option value={option.value} disabled={option.disabled}>{option.label}</option>
			{/each}
		</select>
	{/snippet}
</Field>
