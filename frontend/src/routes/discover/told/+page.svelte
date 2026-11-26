<script lang="ts">
	import DiscoverTabs from '$lib/features/discover/components/DiscoverTabs.svelte';
	import ToldDiscover from '$lib/features/discover/components/ToldDiscover.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
</script>

<svelte:head>
	<title>What you told Discover - DroppedNeedle</title>
</svelte:head>

<ListPageHeader title="What you told Discover" ticket="DN / CC 3.5" lede="Undo any of it.">
	{#snippet below()}
		<DiscoverTabs current="told" />
	{/snippet}
</ListPageHeader>

<div class="dn-told-page">
	<ToldDiscover />
</div>

<style>
	.dn-told-page {
		max-width: 52rem;
		padding: 0.5rem 2rem 4rem;
	}
	@media (max-width: 40rem) {
		.dn-told-page {
			padding: 0.5rem 1rem 3rem;
		}
	}
</style>
