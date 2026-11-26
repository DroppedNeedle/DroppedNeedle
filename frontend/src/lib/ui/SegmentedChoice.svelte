<!--
	One choice from two or three short options, shown side by side as one
	small control. A radio group underneath: one tab stop, arrow keys move,
	`label` is the group's accessible name. `look="caps"` sets the options
	in mono capitals, a size up, for a page header.
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
		look?: 'plain' | 'caps';
		onchange?: (value: T) => void;
	}

	let {
		label,
		options,
		value = $bindable(),
		disabled = false,
		look = 'plain',
		onchange
	}: Props = $props();

	const name = $props.id();

	const group = $derived(look === 'caps' ? 'gap-0.5 rounded-md p-[3px]' : 'rounded-field p-px');
	const option = $derived(
		look === 'caps'
			? 'rounded px-3 py-1.5 font-mono text-[0.6875rem] font-bold uppercase tracking-[0.14em]'
			: 'rounded-field px-2.5 py-0.5 text-xs font-medium'
	);

	function choose(next: T) {
		value = next;
		onchange?.(next);
	}
</script>

<div
	role="radiogroup"
	aria-label={label}
	class="inline-flex shrink-0 border border-line {group} {disabled ? 'opacity-60' : ''}"
>
	{#each options as each (each.value)}
		<label
			class="{option} whitespace-nowrap transition-colors duration-150 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-focus {value ===
			each.value
				? 'bg-primary text-primary-content'
				: 'text-muted hover:text-title'} {disabled ? '' : 'cursor-pointer'}"
		>
			<input
				class="sr-only"
				type="radio"
				{name}
				value={each.value}
				checked={value === each.value}
				{disabled}
				onchange={() => choose(each.value)}
			/>
			{each.label}
		</label>
	{/each}
</div>
