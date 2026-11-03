<script lang="ts">
	interface Props {
		genres: string[];
		selected?: string;
		selectedMultiple?: string[];
		loading?: boolean;
		showAll?: boolean;
		multiSelect?: boolean;
		maxVisible?: number;
		onselect: (genre: string | undefined) => void;
	}

	let {
		genres,
		selected,
		selectedMultiple,
		loading = false,
		showAll = false,
		multiSelect = false,
		maxVisible = 0,
		onselect
	}: Props = $props();

	let expanded = $state(false);

	function isActive(genre: string): boolean {
		if (multiSelect && selectedMultiple) {
			return selectedMultiple.includes(genre);
		}
		return selected === genre;
	}

	function noneSelected(): boolean {
		if (multiSelect && selectedMultiple) {
			return selectedMultiple.length === 0;
		}
		return !selected;
	}

	let visibleGenres = $derived(maxVisible > 0 && !expanded ? genres.slice(0, maxVisible) : genres);
	let hasOverflow = $derived(maxVisible > 0 && genres.length > maxVisible);
</script>

<div class="flex flex-wrap gap-2" role="group" aria-label="Genre filter">
	{#if showAll}
		<button
			class="rounded-full border px-3.5 py-1.5 text-xs font-medium transition-all duration-200
				{noneSelected()
				? 'border-primary/40 bg-primary/15 text-primary shadow-sm shadow-primary/10'
				: 'border-base-content/10 bg-base-100/50 text-base-content/60 hover:border-base-content/20 hover:bg-base-100/80 hover:text-base-content'}"
			disabled={loading}
			aria-pressed={noneSelected()}
			onclick={() => onselect(undefined)}
		>
			All
		</button>
	{/if}
	{#each visibleGenres as genre (genre)}
		<button
			class="rounded-full border px-3.5 py-1.5 text-xs font-medium transition-all duration-200
				{isActive(genre)
				? 'border-primary/40 bg-primary/15 text-primary shadow-sm shadow-primary/10'
				: 'border-base-content/10 bg-base-100/50 text-base-content/60 hover:border-base-content/20 hover:bg-base-100/80 hover:text-base-content'}"
			disabled={loading}
			aria-pressed={isActive(genre)}
			onclick={() => onselect(genre)}
		>
			{genre}
		</button>
	{/each}
	{#if hasOverflow && !expanded}
		<button
			class="rounded-full border border-base-content/10 bg-base-100/50 px-3.5 py-1.5 text-xs font-medium text-base-content/60 hover:border-base-content/20 hover:bg-base-100/80 hover:text-base-content transition-all duration-200"
			onclick={() => (expanded = true)}
		>
			+{genres.length - maxVisible} more
		</button>
	{/if}
</div>
