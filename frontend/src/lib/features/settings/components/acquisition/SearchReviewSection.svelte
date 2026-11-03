<script lang="ts">
	import { SEARCH_REVIEW_OPTIONS } from './qualityRecipeModel';

	interface Props {
		searchReview: string;
	}

	let { searchReview = $bindable() }: Props = $props();
</script>

<div role="radiogroup" aria-label="When to let me pick a copy" class="flex flex-col gap-2">
	{#each SEARCH_REVIEW_OPTIONS as mode (mode.value)}
		<label class="label min-h-11 cursor-pointer justify-start gap-3">
			<input
				type="radio"
				name="acq-search-review"
				class="radio radio-sm"
				value={mode.value}
				bind:group={searchReview}
			/>
			<span class="min-w-0">
				<span>{mode.label}</span>
				<span class="block text-xs whitespace-normal text-base-content/60">{mode.hint}</span>
			</span>
		</label>
	{/each}
</div>
