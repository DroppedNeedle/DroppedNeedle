<script lang="ts">
	import './hub.css';

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

<div class="hub-pills hub-pills--scroll" role="group" aria-label="Genre filter">
	{#if showAll}
		<button
			class="hub-pill"
			disabled={loading}
			aria-pressed={noneSelected()}
			onclick={() => onselect(undefined)}
		>
			All
		</button>
	{/if}
	{#each visibleGenres as genre (genre)}
		<button
			class="hub-pill"
			disabled={loading}
			aria-pressed={isActive(genre)}
			onclick={() => onselect(genre)}
		>
			{genre}
		</button>
	{/each}
	{#if hasOverflow && !expanded}
		<button class="hub-pill" onclick={() => (expanded = true)}>
			+{genres.length - maxVisible} more
		</button>
	{/if}
</div>
