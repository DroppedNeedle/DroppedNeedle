<!--
	Which year the page shows: side by side up to three years, a dropdown
	beyond. Choosing one opens that year's page. Nothing shows while the
	list holds only this year.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import SegmentedChoice from '$lib/ui/SegmentedChoice.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { yearPath } from './yearFormat';

	interface Props {
		/** The years on offer, newest first; the first is the current one. */
		years: number[];
		year: number;
	}

	let { years, year }: Props = $props();

	const options = $derived(years.map((y) => ({ value: String(y), label: String(y) })));
	const current = $derived(years[0] ?? year);

	function open(next: string) {
		const chosen = Number(next);
		if (chosen !== year) void goto(withBasePath(yearPath(chosen, current)));
	}
</script>

{#if years.length > 3}
	<SelectField
		label="Year"
		bare
		size="sm"
		bind:value={() => String(year), open}
		{options}
		class="min-w-28"
	/>
{:else if years.length > 1}
	<SegmentedChoice label="Year" {options} value={String(year)} onchange={open} />
{/if}
