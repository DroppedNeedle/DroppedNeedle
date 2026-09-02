<script lang="ts">
	import { SOURCE_MODE_OPTIONS } from './qualityRecipeModel';

	interface Props {
		sourceSelectionMode: string;
	}

	let { sourceSelectionMode = $bindable() }: Props = $props();
</script>

<div role="radiogroup" aria-label="Source selection mode" class="flex flex-col gap-2">
	{#each SOURCE_MODE_OPTIONS as mode (mode.value)}
		<label class="label min-h-11 cursor-pointer justify-start gap-3">
			<input
				type="radio"
				name="acq-source-mode"
				class="radio radio-sm radio-primary"
				value={mode.value}
				bind:group={sourceSelectionMode}
			/>
			<span class="min-w-0">
				<span class="label-text">{mode.label}</span>
				<span class="block text-xs text-base-content/60">{mode.hint}</span>
			</span>
		</label>
	{/each}
</div>
