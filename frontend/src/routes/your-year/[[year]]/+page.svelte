<script lang="ts">
	import YearNotFound from '$lib/features/year/components/YearNotFound.svelte';
	import YearReviewView from '$lib/features/year/components/YearReviewView.svelte';
	import type { PageProps } from './$types';

	let { params }: PageProps = $props();

	// No year in the address is this year; anything but four digits is no page.
	const year = $derived(
		params.year === undefined
			? new Date().getFullYear()
			: /^\d{4}$/.test(params.year)
				? Number(params.year)
				: null
	);
</script>

{#if year === null}
	<div class="px-4 py-8"><YearNotFound /></div>
{:else}
	<YearReviewView {year} />
{/if}
