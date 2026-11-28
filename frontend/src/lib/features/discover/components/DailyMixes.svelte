<!--
	The Daily Mixes on Discover: one card for each of the person's tastes,
	under the section's head.
-->
<script lang="ts">
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import DailyMixCard from '$lib/features/discover/components/DailyMixCard.svelte';
	import type { DailyMix } from '$lib/features/discover/queries/types';

	interface Props {
		mixes: DailyMix[];
	}

	let { mixes }: Props = $props();
</script>

<SectionHead title="Daily Mixes">
	{#snippet chips()}<span class="dn-mixes__sub">One for each of your tastes</span>{/snippet}
</SectionHead>
<div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
	{#each mixes as mix (mix.id)}
		<DailyMixCard {mix} />
	{/each}
</div>

<style>
	.dn-mixes__sub {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
</style>
