<script lang="ts">
	import { Search, X, ArrowDown } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';

	interface Props {
		searchQuery: string;
		onSearchInput?: () => void;
		placeholder?: string;
		ariaLabel?: string;
		sortOptions?: { value: string; label: string }[];
		sortBy?: string;
		onSortChange?: (value: string) => void;
		sortOrder?: string;
		onToggleSortOrder?: () => void;
		ascValue?: string;
		genres?: string[];
		selectedGenre?: string;
		onGenreChange?: (value: string) => void;
		moods?: string[];
		selectedMood?: string;
		onMoodChange?: (value: string) => void;
		moodLabel?: string;
		decades?: string[];
		selectedDecade?: string;
		onDecadeChange?: (value: string) => void;
		tags?: string[];
		selectedTag?: string;
		onTagChange?: (value: string) => void;
		resultCount?: number | null;
		loading?: boolean;
	}

	let {
		searchQuery = $bindable(),
		onSearchInput,
		placeholder = 'Search albums',
		ariaLabel = 'Search albums',
		sortOptions,
		sortBy,
		onSortChange,
		sortOrder,
		onToggleSortOrder,
		ascValue = 'asc',
		genres,
		selectedGenre,
		onGenreChange,
		moods,
		selectedMood,
		onMoodChange,
		moodLabel = 'Mood',
		decades,
		selectedDecade,
		onDecadeChange,
		tags,
		selectedTag,
		onTagChange,
		resultCount,
		loading = false
	}: Props = $props();

	let isSearching = $derived(searchQuery.trim().length > 0);
	let hasSortControls = $derived(sortOptions && sortOptions.length > 0);
	let hasGenreFilter = $derived(genres && genres.length > 0);
	let hasMoodFilter = $derived(moods && moods.length > 0);
	let hasDecadeChips = $derived(decades && decades.length > 0);
	let hasTagChips = $derived(tags && tags.length > 0);
	let hasSecondRow = $derived(
		hasSortControls || hasGenreFilter || hasMoodFilter || resultCount != null
	);
	let isAscending = $derived(sortOrder === ascValue);

	function clearSearch(): void {
		searchQuery = '';
		onSearchInput?.();
	}

	function handleSortSelect(e: Event): void {
		const value = (e.target as HTMLSelectElement).value;
		onSortChange?.(value);
	}

	function handleGenreSelect(e: Event): void {
		const value = (e.target as HTMLSelectElement).value;
		onGenreChange?.(value);
	}

	function handleMoodSelect(e: Event): void {
		const value = (e.target as HTMLSelectElement).value;
		onMoodChange?.(value);
	}
</script>

<div class="mb-6">
	<div class="relative">
		<Input
			icon={Search}
			label={ariaLabel}
			{placeholder}
			class="w-full pr-12"
			bind:value={searchQuery}
			oninput={() => onSearchInput?.()}
		/>
		{#if isSearching}
			<IconButton
				size="sm"
				icon={X}
				label="Clear search"
				class="absolute right-2 top-1/2 -translate-y-1/2"
				onclick={clearSearch}
			/>
		{/if}
	</div>

	{#if hasSecondRow}
		<div class="flex flex-wrap items-center gap-3 mt-3">
			{#if hasSortControls}
				<select class="select select-sm" onchange={handleSortSelect} aria-label="Sort by">
					{#each sortOptions! as opt (opt.value)}
						<option value={opt.value} selected={sortBy === opt.value}>{opt.label}</option>
					{/each}
				</select>
				<IconButton
					size="sm"
					icon={ArrowDown}
					label={isAscending ? 'Switch to descending sort' : 'Switch to ascending sort'}
					class={isAscending ? '' : '[&_svg]:rotate-180'}
					onclick={() => onToggleSortOrder?.()}
				/>
			{/if}

			{#if hasGenreFilter}
				<select class="select select-sm" onchange={handleGenreSelect} aria-label="Filter by genre">
					<option value="">All genres</option>
					{#each genres! as genre (genre)}
						<option value={genre} selected={selectedGenre === genre}>{genre}</option>
					{/each}
				</select>
			{/if}

			{#if hasMoodFilter}
				<select
					class="select select-sm"
					onchange={handleMoodSelect}
					aria-label="Filter by {moodLabel.toLowerCase()}"
				>
					<option value="">All {moodLabel.toLowerCase()}s</option>
					{#each moods! as mood (mood)}
						<option value={mood} selected={selectedMood === mood}>{mood}</option>
					{/each}
				</select>
			{/if}

			{#if resultCount != null && !loading}
				<span class="text-sm text-base-content/50">{resultCount} results</span>
			{/if}
		</div>
	{/if}

	{#if hasDecadeChips}
		<div class="flex flex-wrap gap-2 mt-3" role="group" aria-label="Filter by decade">
			{@render chip('All', selectedDecade === '', () => onDecadeChange?.(''))}
			{#each decades! as decade (decade)}
				{@render chip(decade, selectedDecade === decade, () => onDecadeChange?.(decade))}
			{/each}
		</div>
	{/if}

	{#if hasTagChips}
		<div class="flex flex-wrap gap-2 mt-3" role="group" aria-label="Filter by tag">
			{@render chip('All tags', selectedTag === '', () => onTagChange?.(''))}
			{#each tags! as tag (tag)}
				{@render chip(tag, selectedTag === tag, () => onTagChange?.(tag))}
			{/each}
		</div>
	{/if}
</div>

{#snippet chip(text: string, selected: boolean, onclick: () => void)}
	<Button variant={selected ? 'primary' : 'ghost'} size="xs" pill aria-pressed={selected} {onclick}>
		{text}
	</Button>
{/snippet}
