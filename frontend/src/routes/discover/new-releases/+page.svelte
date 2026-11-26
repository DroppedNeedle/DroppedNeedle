<script lang="ts">
	import DiscoverTabs from '$lib/features/discover/components/DiscoverTabs.svelte';
	import { Music } from 'lucide-svelte';
	import NewFromYourArtists from '$lib/features/discover/components/NewFromYourArtists.svelte';
	import { getNewReleasesQuery } from '$lib/features/discover/queries/newReleases.svelte';
	import Button from '$lib/ui/Button.svelte';
	import EmptyState from '$lib/ui/EmptyState.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';

	const list = getNewReleasesQuery();
	const releases = $derived(list.data?.pages.flatMap((page) => page.items) ?? []);
	const total = $derived(list.data?.pages[0]?.total ?? null);
	const count = $derived(
		total === null ? null : `${total} ${total === 1 ? 'release' : 'releases'}`
	);
</script>

<svelte:head>
	<title>New from your artists - DroppedNeedle</title>
</svelte:head>

<ListPageHeader
	title="New from your artists"
	ticket="DN / NEW 90"
	{count}
	lede="The last 90 days' releases from artists you play, own or follow."
>
	{#snippet below()}
		<DiscoverTabs current="new-releases" />
	{/snippet}
</ListPageHeader>

<div class="dn-new-releases-page">
	<QueryBoundary query={list} what="your new releases" isEmpty={() => releases.length === 0}>
		{#snippet empty()}
			<EmptyState
				icon={Music}
				title="Nothing new yet"
				description="Releases from artists you play, own or follow show here."
				ctaLabel="Back to Discover"
				ctaHref="/discover"
			/>
		{/snippet}
		{#snippet children(data)}
			<NewFromYourArtists releases={data.pages.flatMap((page) => page.items)} />
			{#if list.hasNextPage}
				<div class="dn-new-releases-page__more">
					<Button
						variant="outline"
						size="sm"
						loading={list.isFetchingNextPage}
						onclick={() => list.fetchNextPage()}
					>
						Show more
					</Button>
				</div>
			{/if}
		{/snippet}
	</QueryBoundary>
</div>

<style>
	.dn-new-releases-page {
		max-width: 64rem;
		padding: 0.5rem 2rem 4rem;
	}
	.dn-new-releases-page__more {
		display: flex;
		justify-content: center;
		margin-top: 1rem;
	}
	@media (max-width: 40rem) {
		.dn-new-releases-page {
			padding: 0.5rem 1rem 3rem;
		}
	}
</style>
