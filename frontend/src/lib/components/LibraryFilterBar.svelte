<script lang="ts">
	import { Search, X, ArrowDown } from 'lucide-svelte';

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
	<div class="group relative max-w-xl">
		<Search
			class="pointer-events-none absolute left-4 top-1/2 h-4 w-4 -translate-y-1/2 text-base-content/40
			transition-colors duration-200 group-focus-within:text-primary"
		/>
		<input
			type="text"
			{placeholder}
			class="input input-md w-full rounded-full border-base-content/10 bg-base-200/50
				pl-11 pr-12 transition-all duration-200
				placeholder:text-base-content/30
				focus:border-primary/40 focus:bg-base-200/80"
			bind:value={searchQuery}
			oninput={() => onSearchInput?.()}
			aria-label={ariaLabel}
		/>
		{#if isSearching}
			<button
				type="button"
				class="btn btn-ghost btn-sm btn-circle absolute right-2 top-1/2 -translate-y-1/2"
				onclick={clearSearch}
				aria-label="Clear search"
			>
				<X class="h-4 w-4" />
			</button>
		{/if}
	</div>

	{#if hasSecondRow}
		<div class="mt-3 flex flex-wrap items-center gap-2.5">
			{#if hasSortControls}
				<select
					class="select select-sm rounded-full border-base-content/10 bg-base-200/50
						transition-all duration-200 hover:border-primary/30 focus:border-primary/40"
					onchange={handleSortSelect}
					aria-label="Sort by"
				>
					{#each sortOptions! as opt (opt.value)}
						<option value={opt.value} selected={sortBy === opt.value}>{opt.label}</option>
					{/each}
				</select>
				<button
					type="button"
					class="btn btn-ghost btn-sm btn-circle border border-base-content/10 bg-base-200/50 transition-colors hover:border-primary/30"
					onclick={() => onToggleSortOrder?.()}
					aria-label={isAscending ? 'Switch to descending sort' : 'Switch to ascending sort'}
					title={isAscending ? 'Ascending' : 'Descending'}
				>
					<ArrowDown class="h-4 w-4 transition-transform {isAscending ? '' : 'rotate-180'}" />
				</button>
			{/if}

			{#if hasGenreFilter}
				<select
					class="select select-sm rounded-full border-base-content/10 bg-base-200/50
						transition-all duration-200 hover:border-primary/30 focus:border-primary/40"
					onchange={handleGenreSelect}
					aria-label="Filter by genre"
				>
					<option value="">All genres</option>
					{#each genres! as genre (genre)}
						<option value={genre} selected={selectedGenre === genre}>{genre}</option>
					{/each}
				</select>
			{/if}

			{#if hasMoodFilter}
				<select
					class="select select-sm rounded-full border-base-content/10 bg-base-200/50
						transition-all duration-200 hover:border-primary/30 focus:border-primary/40"
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
				<span
					class="rounded-full border border-base-content/8 bg-base-200/50 px-3 py-1 font-mono text-[0.62rem] font-bold uppercase tracking-[0.18em] tabular-nums text-base-content/50"
					>{resultCount} results</span
				>
			{/if}
		</div>
	{/if}

	{#if hasDecadeChips}
		<div class="mt-3 flex flex-wrap gap-2" role="group" aria-label="Filter by decade">
			<button
				type="button"
				class="btn btn-xs rounded-full font-mono uppercase tracking-wider {selectedDecade === ''
					? 'btn-primary'
					: 'btn-ghost border border-base-content/10 bg-base-200/50 text-base-content/60 hover:border-primary/30'}"
				onclick={() => onDecadeChange?.('')}
			>
				All
			</button>
			{#each decades! as decade (decade)}
				<button
					type="button"
					class="btn btn-xs rounded-full font-mono uppercase tracking-wider {selectedDecade ===
					decade
						? 'btn-primary'
						: 'btn-ghost border border-base-content/10 bg-base-200/50 text-base-content/60 hover:border-primary/30'}"
					onclick={() => onDecadeChange?.(decade)}
				>
					{decade}
				</button>
			{/each}
		</div>
	{/if}

	{#if hasTagChips}
		<div class="mt-3 flex flex-wrap gap-2" role="group" aria-label="Filter by tag">
			<button
				type="button"
				class="btn btn-xs rounded-full font-mono uppercase tracking-wider {selectedTag === ''
					? 'btn-primary'
					: 'btn-ghost border border-base-content/10 bg-base-200/50 text-base-content/60 hover:border-primary/30'}"
				onclick={() => onTagChange?.('')}
			>
				All tags
			</button>
			{#each tags! as tag (tag)}
				<button
					type="button"
					class="btn btn-xs rounded-full font-mono uppercase tracking-wider {selectedTag === tag
						? 'btn-primary'
						: 'btn-ghost border border-base-content/10 bg-base-200/50 text-base-content/60 hover:border-primary/30'}"
					onclick={() => onTagChange?.(tag)}
				>
					{tag}
				</button>
			{/each}
		</div>
	{/if}
</div>
