<!--
	The ways from Discover to its own pages (`DISCOVER_PAGES`) and to What
	you told Discover, in one place: `caps` is the header's row of caps
	links, `rows` the phone's ruled rows under the bin. Later shows only
	while the pile holds albums, with their count.
-->
<script lang="ts">
	import { ChevronRight } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { DISCOVER_PAGES, type DiscoverPage } from '$lib/features/discover/state/discoverPages';

	interface Props {
		look: 'caps' | 'rows';
		/** Albums on the Later pile. */
		laterCount: number;
		ontold: () => void;
		/** Layout only: margin. */
		class?: string;
	}

	let { look, laterCount, ontold, class: className = '' }: Props = $props();

	const pages = $derived(
		DISCOVER_PAGES.filter((page) => page.id !== 'later' || laterCount > 0).map(
			(page: DiscoverPage) => ({
				...page,
				label: page.id === 'later' ? `${page.label} (${laterCount})` : page.label
			})
		)
	);
</script>

<nav class="dn-discover-links dn-discover-links--{look} {className}" aria-label="Discover">
	{#each pages as page (page.id)}
		{#if look === 'caps'}
			<Button variant="caps" size="sm" href={withBasePath(page.path)}>{page.label}</Button>
		{:else}
			<a href={withBasePath(page.path)}>
				<page.icon class="size-4" aria-hidden="true" />
				<span>{page.label}</span>
				<ChevronRight class="ml-auto size-4" aria-hidden="true" />
			</a>
		{/if}
	{/each}
	{#if look === 'caps'}
		<Button variant="caps" size="sm" onclick={ontold}>What you told Discover</Button>
	{:else}
		<div class="dn-discover-links__row">
			<Button
				variant="caps"
				size="sm"
				icon={ChevronRight}
				iconPosition="end"
				class="w-full justify-between"
				onclick={ontold}
			>
				What you told Discover
			</Button>
		</div>
	{/if}
</nav>

<style>
	.dn-discover-links--caps {
		display: flex;
		gap: 0.25rem;
	}
	.dn-discover-links--rows {
		display: flex;
		flex-direction: column;
	}
	.dn-discover-links--rows a,
	.dn-discover-links__row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		min-height: 3rem;
		padding: 0 0.25rem;
		border-bottom: 1px solid var(--dn-line);
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-align: left;
		text-transform: uppercase;
	}
	.dn-discover-links--rows > :first-child {
		border-top: 1px solid var(--dn-line);
	}
</style>
