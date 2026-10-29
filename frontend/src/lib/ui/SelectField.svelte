<!--
	A labelled dropdown over a list of options.
	Replaces the hand-written `select select-bordered` blocks.
	`bare` drops the visible label (it stays the accessible name), for a
	small dropdown inside a table row or a toolbar.
-->
<script lang="ts" generics="T extends string">
	import type { HTMLSelectAttributes } from 'svelte/elements';
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
		/** No visible label, help or error line: just the dropdown. */
		bare?: boolean;
		size?: 'sm' | 'md';
		/** Extra classes on the dropdown, e.g. a minimum width for a bare one. */
		class?: string;
		ref?: HTMLSelectElement | null;
	}

	let {
		label,
		value = $bindable(),
		options,
		help,
		error,
		disabled = false,
		bare = false,
		size = 'md',
		class: className = '',
		ref = $bindable(null)
	}: Props = $props();
</script>

{#snippet dropdown(
	control: Pick<HTMLSelectAttributes, 'id' | 'aria-label' | 'aria-describedby' | 'aria-invalid'>
)}
	<select
		{...control}
		class={[
			'select',
			bare ? 'w-auto' : 'w-full',
			size === 'sm' && 'select-sm',
			error && 'select-error',
			className
		]}
		{disabled}
		bind:value
		bind:this={ref}
	>
		{#each options as option (option.value)}
			<option value={option.value} disabled={option.disabled}>{option.label}</option>
		{/each}
	</select>
{/snippet}

{#if bare}
	{@render dropdown({ 'aria-label': label })}
{:else}
	<Field {label} {help} {error}>
		{#snippet children(control)}
			{@render dropdown(control)}
		{/snippet}
	</Field>
{/if}
