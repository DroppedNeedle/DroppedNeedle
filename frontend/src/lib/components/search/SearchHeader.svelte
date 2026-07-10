<script lang="ts">
	/*
	 * SearchHeader — shared masthead for the three search result pages:
	 * the query as the headline plus the All / Artists / Albums bucket rail.
	 */
	import { goto } from '$app/navigation';
	import { Search } from 'lucide-svelte';

	interface Props {
		query: string;
		active: 'all' | 'artists' | 'albums';
	}

	let { query, active }: Props = $props();

	const buckets: { id: 'all' | 'artists' | 'albums'; label: string }[] = [
		{ id: 'all', label: 'All' },
		{ id: 'artists', label: 'Artists' },
		{ id: 'albums', label: 'Albums' }
	];

	function navigate(bucket: 'all' | 'artists' | 'albums') {
		if (bucket === active || !query) return;
		const base = bucket === 'all' ? '/search' : `/search/${bucket}`;
		goto(`${base}?q=${encodeURIComponent(query)}`);
	}
</script>

<header class="px-4 pt-6 pb-4 sm:px-6 lg:px-8">
	<p
		class="mb-1 flex items-center gap-2 font-mono text-[0.7rem] uppercase tracking-[0.16em] text-base-content/50"
	>
		<Search class="h-3.5 w-3.5" />
		Search results
	</p>
	<h1 class="hero-title font-display text-2xl font-bold sm:text-3xl">
		“{query}”
	</h1>

	<div class="mt-4 flex gap-1.5" role="tablist" aria-label="Result type">
		{#each buckets as bucket (bucket.id)}
			<button
				role="tab"
				aria-selected={active === bucket.id}
				class="btn btn-sm rounded-full px-4 {active === bucket.id
					? 'btn-primary'
					: 'btn-ghost bg-base-content/6 hover:bg-base-content/12'}"
				onclick={() => navigate(bucket.id)}
			>
				{bucket.label}
			</button>
		{/each}
	</div>
</header>
