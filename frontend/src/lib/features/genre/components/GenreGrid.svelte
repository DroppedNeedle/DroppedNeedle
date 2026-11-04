<!--
	Home's genres as printed tiles: the first nine and a "More" tile that
	opens the rest in place. Fills alternate paper and acid in a checker, so
	no two neighbours match at any column count.
-->
<script lang="ts">
	import { ArrowRight, ArrowUp } from 'lucide-svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import GenreTile from './GenreTile.svelte';

	interface Props {
		title: string;
		genres: { name: string; listen_count?: number | null }[];
	}

	let { title, genres }: Props = $props();

	const FIRST = 9;
	const all = $derived(genres.slice(0, 20));
	// a "More" that would open one tile is not worth the tap
	const folds = $derived(all.length > FIRST + 1);
	let open = $state(false);
	const shown = $derived(folds && !open ? all.slice(0, FIRST) : all);

	function formatCount(n: number): string {
		if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
		if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
		return n.toString();
	}
</script>

<section>
	<SectionHead {title} />
	<div class="genre-grid grid grid-cols-2 gap-2.5 sm:grid-cols-3 sm:gap-3 lg:grid-cols-5">
		{#each shown as genre (genre.name)}
			<GenreTile
				name={genre.name}
				plays={genre.listen_count ? formatCount(genre.listen_count) : null}
			/>
		{/each}
		{#if folds}
			<button type="button" class="genre-more" aria-expanded={open} onclick={() => (open = !open)}>
				{#if open}
					Fewer<span class="sr-only"> genres</span>
					<ArrowUp class="size-5" aria-hidden="true" />
				{:else}
					More<span class="sr-only"> genres</span>
					<ArrowRight class="size-5" aria-hidden="true" />
				{/if}
			</button>
		{/if}
	</div>
</section>

<style>
	/* acid tiles in a checker: 2 columns on phones, odd counts above */
	@media (max-width: 639px) {
		.genre-grid > :global(.genre-tile:nth-child(4n + 2)),
		.genre-grid > :global(.genre-tile:nth-child(4n + 3)) {
			--tile-bg: var(--dn-action);
			--tile-ink: var(--dn-action-content);
			--tile-edge: var(--dn-action-edge);
		}
	}
	@media (min-width: 640px) {
		.genre-grid > :global(.genre-tile:nth-child(even)) {
			--tile-bg: var(--dn-action);
			--tile-ink: var(--dn-action-content);
			--tile-edge: var(--dn-action-edge);
		}
	}

	.genre-more {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		min-height: 4rem;
		border: 2px solid var(--dn-edge);
		border-radius: 0.375rem;
		background: var(--color-base-100);
		color: var(--dn-title);
		font-family: var(--font-display);
		font-size: 1.5rem;
		line-height: 1;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		font-synthesis: none;
		transition:
			border-color 150ms,
			background-color 150ms;
	}
	.genre-more:hover {
		border-color: var(--dn-title);
		background: var(--color-base-200);
	}
</style>
