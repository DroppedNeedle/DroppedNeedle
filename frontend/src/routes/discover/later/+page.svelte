<script lang="ts">
	import LaterPile from '$lib/features/discover/components/LaterPile.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import { withBasePath } from '$lib/utils/basePath';
</script>

<svelte:head>
	<title>Later - DroppedNeedle</title>
</svelte:head>

<ListPageHeader
	title="Later"
	ticket="DN / CC 3.2"
	lede="Albums you put aside. Each one leaves by itself once it is in your library."
	backHref={withBasePath('/discover')}
	backLabel="Back to Discover"
	backStyle="link"
/>

<div class="dn-later-page">
	<LaterPile />
</div>

<style>
	.dn-later-page {
		max-width: 64rem;
		padding: 0.5rem 2rem 4rem;
	}
	@media (max-width: 40rem) {
		.dn-later-page {
			padding: 0.5rem 1rem 3rem;
		}
	}
</style>
