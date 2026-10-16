<!--
	One on/off setting: a label and optional description on the left, the
	switch on the right, all of it one click target.
	Replaces the hand-written `<input type="checkbox" class="toggle">` rows.
-->
<script lang="ts">
	interface Props {
		label: string;
		description?: string;
		checked: boolean;
		disabled?: boolean;
	}

	let { label, description, checked = $bindable(false), disabled = false }: Props = $props();

	const id = $props.id();
</script>

<div class="flex items-start justify-between gap-3">
	<div class="min-w-0">
		<label for={id} class="block text-sm font-medium {disabled ? '' : 'cursor-pointer'}">
			{label}
		</label>
		{#if description}
			<p id="{id}-description" class="mt-0.5 text-sm text-base-content/60">{description}</p>
		{/if}
	</div>
	<input
		{id}
		type="checkbox"
		class="toggle toggle-sm toggle-primary shrink-0"
		aria-describedby={description ? `${id}-description` : undefined}
		{disabled}
		bind:checked
	/>
</div>
