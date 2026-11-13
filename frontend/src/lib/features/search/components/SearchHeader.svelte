<!--
	The search pages' header: "Search results for" over the query in the list
	header's giant title, and the four tabs under it. With no query it is
	the plain "Search" title.
-->
<script lang="ts">
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	type Tab = 'all' | 'artists' | 'albums' | 'tracks';

	interface Props {
		query: string;
		current: Tab;
	}

	let { query, current }: Props = $props();

	const tabs: { id: Tab; label: string; path: string }[] = [
		{ id: 'all', label: 'All', path: '/search' },
		{ id: 'artists', label: 'Artists', path: '/search/artists' },
		{ id: 'albums', label: 'Albums', path: '/search/albums' },
		{ id: 'tracks', label: 'Songs', path: '/search/tracks' }
	];

	const hasQuery = $derived(query.length > 0);
</script>

<svelte:head><title>{hasQuery ? `Search: ${query}` : 'Search'} - DroppedNeedle</title></svelte:head>

{#snippet tabsRow()}
	<nav class="search-tabs" aria-label="Search results">
		{#each tabs as tab (tab.id)}
			<Button
				href={withBasePath(`${tab.path}?q=${encodeURIComponent(query)}`)}
				variant={tab.id === current ? 'primary' : 'outline'}
				size="sm"
				pill
				aria-current={tab.id === current ? 'page' : undefined}>{tab.label}</Button
			>
		{/each}
	</nav>
{/snippet}

<ListPageHeader
	title={hasQuery ? query : 'Search'}
	eyebrow={hasQuery ? 'Search results for' : undefined}
	fitTitle={hasQuery}
	ticket="DN / CC 3.2 - SRCH"
	below={hasQuery ? tabsRow : undefined}
/>

<style>
	.search-tabs {
		display: flex;
		gap: 0.625rem;
		margin: -3px;
		padding: 3px;
		overflow-x: auto;
		scrollbar-width: none;
	}
	.search-tabs :global(.btn) {
		flex: none;
		min-width: 4.5rem;
		--fontsize: 0.9375rem;
	}
</style>
