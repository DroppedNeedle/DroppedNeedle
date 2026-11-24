<!--
	One on/off setting: a label and optional description on the left, the
	switch on the right, all of it one click target.
	Replaces the hand-written `<input type="checkbox" class="toggle">` rows.
	`bare` shows only the switch (the label stays its accessible name), for a
	switch inside a table row. `leading` puts the switch before its label, as
	wide as the two need, for a switch inside a slim row.
-->
<script lang="ts">
	interface Props {
		label: string;
		description?: string;
		checked: boolean;
		disabled?: boolean;
		/** Just the switch, no visible label or description. */
		bare?: boolean;
		/** The switch first, then the label, not stretched across the row. */
		leading?: boolean;
	}

	let {
		label,
		description,
		checked = $bindable(false),
		disabled = false,
		bare = false,
		leading = false
	}: Props = $props();

	const id = $props.id();
</script>

{#if bare}
	<input
		type="checkbox"
		class="toggle toggle-sm toggle-primary shrink-0"
		aria-label={label}
		title={label}
		{disabled}
		bind:checked
	/>
{:else if leading}
	<label
		class="inline-flex shrink-0 items-center gap-2 text-sm font-medium {disabled
			? ''
			: 'cursor-pointer'}"
	>
		<input
			type="checkbox"
			class="toggle toggle-sm toggle-primary shrink-0"
			{disabled}
			bind:checked
		/>
		{label}
	</label>
{:else}
	<div class="flex w-full items-start justify-between gap-3">
		<div class="min-w-0">
			<label for={id} class="block text-sm font-medium {disabled ? '' : 'cursor-pointer'}">
				{label}
			</label>
			{#if description}
				<p id="{id}-description" class="mt-0.5 text-sm text-muted">{description}</p>
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
{/if}
