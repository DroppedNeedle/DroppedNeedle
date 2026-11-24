<!--
	One choice from two or three short options, shown side by side as one
	small control. A radio group underneath: one tab stop, arrow keys move,
	`label` is the group's accessible name.
-->
<script lang="ts" generics="T extends string">
	interface Option {
		value: T;
		label: string;
	}

	interface Props {
		label: string;
		options: Option[];
		value: T;
		disabled?: boolean;
		onchange?: (value: T) => void;
	}

	let { label, options, value = $bindable(), disabled = false, onchange }: Props = $props();

	const name = $props.id();

	function choose(next: T) {
		value = next;
		onchange?.(next);
	}
</script>

<div
	role="radiogroup"
	aria-label={label}
	class="inline-flex shrink-0 rounded-field border border-line p-px {disabled ? 'opacity-60' : ''}"
>
	{#each options as option (option.value)}
		<label
			class="rounded-field px-2.5 py-0.5 text-xs font-medium whitespace-nowrap transition-colors duration-150 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-focus {value ===
			option.value
				? 'bg-primary text-primary-content'
				: 'text-muted hover:text-title'} {disabled ? '' : 'cursor-pointer'}"
		>
			<input
				class="sr-only"
				type="radio"
				{name}
				value={option.value}
				checked={value === option.value}
				{disabled}
				onchange={() => choose(option.value)}
			/>
			{option.label}
		</label>
	{/each}
</div>
