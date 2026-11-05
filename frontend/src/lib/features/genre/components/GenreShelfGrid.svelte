<!--
	A genre page grid that fits its tiles to its own width: artists three
	across and albums two on a phone (two artists on the narrowest), up to
	eight on a wide panel. With `rows` it shows that many rows (at least
	`minShown` tiles) and folds the rest: behind "View all" in the `title`
	heading, or, with `moreLabel`, behind one button at the end that first
	opens the rows already loaded and then calls `onmore` for the next page.
	"Show less" folds them back.
-->
<script lang="ts" generics="T">
	import type { Snippet } from 'svelte';
	import { ArrowDown, ArrowRight, ArrowUp } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		kind: 'artist' | 'album';
		items: T[];
		key: (item: T) => string;
		item: Snippet<[T]>;
		/** The small heading over the grid ("Artists"). */
		title?: string;
		/** Rows shown folded; every item when not given. */
		rows?: number;
		/** Rows shown folded on a phone; `rows` when not given. */
		phoneRows?: number;
		/** The fewest tiles shown folded, for a narrow grid. */
		minShown?: number;
		/** The end button's words ("View more artists"), for a paged list. */
		moreLabel?: string;
		/** Another page can be fetched. */
		hasMore?: boolean;
		loadingMore?: boolean;
		onmore?: () => void;
	}

	let {
		kind,
		items,
		key,
		item,
		title,
		rows,
		phoneRows,
		minShown = 0,
		moreLabel,
		hasMore = false,
		loadingMore = false,
		onmore
	}: Props = $props();

	const GAP = 16;
	const MIN_TILE = { artist: 88, album: 96 };

	let width = $state(0);
	let open = $state(false);
	const gridId = $props.id();

	// a 320px phone keeps two artists a row, so their names have room
	const cols = $derived(
		width < 480
			? kind === 'artist' && width >= 300
				? 3
				: 2
			: Math.min(8, Math.max(3, Math.floor((width + GAP) / (MIN_TILE[kind] + GAP))))
	);
	const rowsShown = $derived(width < 480 ? (phoneRows ?? rows) : rows);
	const limit = $derived(rowsShown ? Math.max(rowsShown * cols, minShown) : items.length);
	const folds = $derived(items.length > limit);
	const shown = $derived(folds && !open ? items.slice(0, limit) : items);

	function more() {
		if (folds && !open) open = true;
		else onmore?.();
	}
</script>

<div class="genre-shelf">
	{#if title}
		<div class="genre-shelf__head">
			<h3 class="genre-shelf__title">{title}</h3>
			{#if folds}
				<Button
					variant="caps"
					size="xs"
					icon={open ? ArrowUp : ArrowRight}
					iconPosition="end"
					aria-expanded={open}
					aria-controls={gridId}
					onclick={() => (open = !open)}
				>
					{open ? 'Show less' : 'View all'}<span class="sr-only"> {title}</span>
				</Button>
			{/if}
		</div>
	{/if}
	<div
		id={gridId}
		class="genre-shelf__grid"
		bind:clientWidth={width}
		style:grid-template-columns="repeat({cols}, minmax(0, 1fr))"
	>
		{#each shown as entry (key(entry))}
			{@render item(entry)}
		{/each}
	</div>
	{#if moreLabel && (folds || hasMore)}
		<div class="genre-shelf__end">
			{#if (folds && !open) || hasMore}
				<Button
					variant="outline"
					size="display-sm"
					icon={ArrowDown}
					iconPosition="end"
					loading={loadingMore}
					onclick={more}
				>
					{moreLabel}
				</Button>
			{/if}
			{#if folds && open}
				<Button
					variant="caps"
					size="xs"
					icon={ArrowUp}
					iconPosition="end"
					aria-expanded={open}
					aria-controls={gridId}
					onclick={() => (open = false)}
				>
					Show less
				</Button>
			{/if}
		</div>
	{/if}
</div>

<style>
	.genre-shelf__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		min-height: 2rem;
		margin-bottom: 0.5rem;
	}

	.genre-shelf__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 1.375rem;
		line-height: 1;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		font-synthesis: none;
	}

	.genre-shelf__head :global(.btn) {
		margin-right: -0.5rem;
		letter-spacing: 0.16em;
	}

	.genre-shelf__end {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: center;
		gap: 0.75rem 1.25rem;
		padding-top: 1.5rem;
	}

	.genre-shelf__grid {
		display: grid;
		gap: 1rem 1rem;
	}
</style>
