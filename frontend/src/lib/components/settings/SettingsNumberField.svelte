<script lang="ts">
	let {
		label,
		description,
		value = $bindable(0),
		min = 0,
		max = 999999,
		step = 1,
		unit = ''
	}: {
		label: string;
		description: string;
		value: number;
		min?: number;
		max?: number;
		step?: number;
		unit?: string;
	} = $props();
</script>

<fieldset class="fieldset">
	<legend
		class="fieldset-legend font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
	>
		{label}
	</legend>
	<label class="input w-full rounded-xl border-base-content/8 bg-base-200/50">
		<input type="number" bind:value {min} {max} {step} class="grow" />
		{#if unit}
			<span class="label font-mono text-[0.62rem] uppercase tracking-[0.14em] text-base-content/40"
				>{unit}</span
			>
		{/if}
	</label>
	<p class="label text-base-content/50">{description}</p>
</fieldset>
