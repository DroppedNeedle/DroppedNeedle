<!--
	One choice from a few, each a card with a radio, a label and an optional
	line under it, all of it one click target. `label` names the group for
	screen readers. `class` is for layout only, such as the grid's columns.
-->
<script lang="ts" generics="T extends string">
	interface Option {
		value: T;
		label: string;
		description?: string;
	}

	interface Props {
		label: string;
		options: Option[];
		value: T;
		onchange?: (value: T) => void;
		class?: string;
	}

	let { label, options, value = $bindable(), onchange, class: className = '' }: Props = $props();

	const name = $props.id();

	function choose(next: T) {
		value = next;
		onchange?.(next);
	}
</script>

<fieldset class="grid gap-3 {className}">
	<legend class="sr-only">{label}</legend>
	{#each options as option (option.value)}
		<label
			class="flex cursor-pointer items-start gap-3 rounded-lg border p-4 transition-colors {value ===
			option.value
				? 'border-primary/50 bg-primary/5'
				: 'border-base-300/50 bg-base-100/30 hover:border-base-content/20'}"
		>
			<input
				class="radio radio-primary radio-sm mt-0.5"
				type="radio"
				{name}
				value={option.value}
				checked={value === option.value}
				onchange={() => choose(option.value)}
			/>
			<span>
				<span class="block text-sm font-medium">{option.label}</span>
				{#if option.description}
					<span class="mt-1 block text-xs text-base-content/60">{option.description}</span>
				{/if}
			</span>
		</label>
	{/each}
</fieldset>
