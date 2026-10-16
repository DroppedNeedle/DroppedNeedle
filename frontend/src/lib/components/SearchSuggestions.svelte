<script lang="ts">
	import { Search } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Input from '$lib/ui/Input.svelte';
	import type { SearchRemoteStatus, SuggestResult } from '$lib/types';
	import { getSearchSuggestionsV3Query } from '$lib/queries/search/SearchV3Queries.svelte';
	import { toSearchRemoteStatus, toSuggestResultsV1 } from '$lib/queries/search/SearchV3Adapters';

	interface Props {
		query: string;
		onSearch: () => void;
		onSelect: (result: SuggestResult) => void;
		placeholder?: string;
		inputClass?: string;
		autofocus?: boolean;
		id?: string;
	}

	let {
		query = $bindable(),
		onSearch,
		onSelect,
		placeholder = 'Search...',
		inputClass = '',
		autofocus = false,
		id = 'suggest'
	}: Props = $props();

	const listboxId = $derived(`${id}-listbox`);

	let showDropdown = $state(false);
	let activeIndex = $state(-1);
	let debounceTimeout: ReturnType<typeof setTimeout>;
	let rootRef: HTMLDivElement;
	let debouncedQuery = $state('');
	let queryEnabled = $state(false);

	// The v3 suggest endpoint already merges the local catalog across buckets,
	// best first, so one call replaces the old remote-plus-two-local fan-out.
	// Track rows drop in the adapter, so over-fetch to keep five artist/album
	// rows in the dropdown.
	const SUGGEST_FETCH_LIMIT = 10;
	const SUGGEST_DISPLAY_LIMIT = 5;

	const suggestQuery = getSearchSuggestionsV3Query(
		() => debouncedQuery,
		() => queryEnabled,
		SUGGEST_FETCH_LIMIT
	);

	let suggestions = $derived(
		toSuggestResultsV1(suggestQuery.data?.results ?? []).slice(0, SUGGEST_DISPLAY_LIMIT)
	);
	let remoteStatus: SearchRemoteStatus = $derived(
		suggestQuery.isError ? 'error' : toSearchRemoteStatus(suggestQuery.data?.status ?? 'ok')
	);
	let waitingForDebounce = $derived(
		showDropdown && query.trim().length >= 2 && debouncedQuery !== query.trim()
	);
	let loading = $derived(waitingForDebounce || (queryEnabled && suggestQuery.isFetching));

	const activeDescendant = $derived(
		activeIndex >= 0 && activeIndex < suggestions.length ? `${id}-option-${activeIndex}` : undefined
	);

	// A local-only album uses its local id in place of a MusicBrainz id, so it has no cover to fetch.
	function isLocalOnly(result: SuggestResult): boolean {
		return Boolean(result.local_id && result.local_id === result.musicbrainz_id);
	}

	function handleInput() {
		clearTimeout(debounceTimeout);
		queryEnabled = false;
		debouncedQuery = '';
		activeIndex = -1;

		if (query.trim().length < 2) {
			showDropdown = false;
			return;
		}

		showDropdown = true;

		debounceTimeout = setTimeout(() => {
			debouncedQuery = query.trim();
			queryEnabled = true;
		}, 300);
	}

	function closeDropdown() {
		showDropdown = false;
		queryEnabled = false;
		debouncedQuery = '';
		activeIndex = -1;
	}

	function handleSubmit(e: SubmitEvent) {
		e.preventDefault();
		closeDropdown();
		onSearch();
	}

	function handleSelect(result: SuggestResult) {
		closeDropdown();
		onSelect(result);
	}

	function handleViewAll() {
		closeDropdown();
		onSearch();
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			if (showDropdown) {
				e.preventDefault();
				e.stopPropagation();
				showDropdown = false;
				queryEnabled = false;
				debouncedQuery = '';
				activeIndex = -1;
			}
			return;
		}

		if (!showDropdown || suggestions.length === 0) return;

		switch (e.key) {
			case 'ArrowDown':
				e.preventDefault();
				activeIndex = activeIndex < suggestions.length - 1 ? activeIndex + 1 : 0;
				break;
			case 'ArrowUp':
				e.preventDefault();
				activeIndex = activeIndex > 0 ? activeIndex - 1 : suggestions.length - 1;
				break;
			case 'Home':
				if (activeIndex >= 0) {
					e.preventDefault();
					activeIndex = 0;
				}
				break;
			case 'End':
				if (activeIndex >= 0) {
					e.preventDefault();
					activeIndex = suggestions.length - 1;
				}
				break;
			case 'Enter':
				if (activeIndex >= 0 && activeIndex < suggestions.length) {
					e.preventDefault();
					handleSelect(suggestions[activeIndex]);
				}
				break;
		}
	}

	function handleFocusOut(e: FocusEvent) {
		if (rootRef && !rootRef.contains(e.relatedTarget as Node)) {
			showDropdown = false;
		}
	}

	$effect(() => {
		if (!showDropdown) return;
		const handlePointerDown = (e: PointerEvent) => {
			if (rootRef && !rootRef.contains(e.target as Node)) {
				showDropdown = false;
			}
		};
		document.addEventListener('pointerdown', handlePointerDown);
		return () => document.removeEventListener('pointerdown', handlePointerDown);
	});

	$effect(() => {
		return () => {
			clearTimeout(debounceTimeout);
		};
	});
</script>

<div
	bind:this={rootRef}
	class="relative w-full"
	role="combobox"
	aria-expanded={showDropdown}
	aria-haspopup="listbox"
	aria-controls={listboxId}
	onfocusout={handleFocusOut}
>
	<form onsubmit={handleSubmit}>
		<div class="relative">
			<Input
				type="search"
				label="Search"
				icon={Search}
				{placeholder}
				bind:value={query}
				oninput={handleInput}
				onkeydown={handleKeydown}
				class="w-full {inputClass}"
				autocomplete="off"
				aria-autocomplete="list"
				aria-controls={listboxId}
				aria-activedescendant={activeDescendant}
				{autofocus}
			/>
			{#if loading}
				<span
					class="loading loading-spinner loading-sm pointer-events-none absolute right-3 top-1/2 -translate-y-1/2"
					aria-hidden="true"
				></span>
			{/if}
		</div>
	</form>

	{#if showDropdown && (suggestions.length > 0 || loading || remoteStatus !== 'ok')}
		<ul
			role="listbox"
			id={listboxId}
			class="absolute top-full left-0 right-0 z-60 mt-1 rounded-box bg-base-200 shadow-xl"
		>
			{#each suggestions as result, i (result.musicbrainz_id)}
				<li
					role="option"
					id="{id}-option-{i}"
					aria-selected={i === activeIndex}
					class="flex items-center gap-3 p-3 cursor-pointer hover:bg-base-300 transition-colors {i ===
					activeIndex
						? 'bg-base-300'
						: ''}"
					onclick={() => handleSelect(result)}
					onkeydown={(e) => {
						if (e.key === 'Enter' || e.key === ' ') handleSelect(result);
					}}
					tabindex="-1"
				>
					{#if result.type === 'artist'}
						<ArtistImage
							mbid={result.musicbrainz_id}
							alt={result.title}
							size="full"
							requestSize={250}
							lazy={false}
							rounded="sm"
							available={!isLocalOnly(result)}
							className="w-10 h-10"
						/>
					{:else}
						<AlbumImage
							mbid={result.musicbrainz_id}
							alt={result.title}
							size="full"
							requestSize={250}
							lazy={false}
							rounded="sm"
							available={!isLocalOnly(result)}
							retryOnError={false}
							className="w-10 h-10"
						/>
					{/if}
					<div class="flex-1 min-w-0">
						<div class="font-medium truncate">{result.title}</div>
						<div class="text-sm opacity-70 truncate">
							{#if result.type === 'album' && result.artist}
								{result.artist}
							{:else if result.type === 'artist'}
								Artist
							{/if}
							{#if result.year}
								&middot; {result.year}
							{/if}
							{#if result.disambiguation}
								({result.disambiguation})
							{/if}
						</div>
					</div>
					<div class="flex gap-1">
						<span class="badge badge-sm badge-ghost">
							{result.type === 'artist' ? 'Artist' : 'Album'}
						</span>
						{#if result.in_library}
							<span class="badge badge-sm badge-success">In Library</span>
						{/if}
						{#if result.requested}
							<span class="badge badge-sm badge-warning">Requested</span>
						{/if}
					</div>
				</li>
			{/each}

			{#if suggestions.length > 0}
				<li class="p-3 text-center border-t border-base-300">
					<button class="text-sm link link-hover opacity-70" onclick={handleViewAll}>
						View all results
					</button>
				</li>
			{/if}

			{#if loading && suggestions.length === 0}
				<li class="p-4 flex justify-center">
					<span class="loading loading-spinner loading-md"></span>
				</li>
			{/if}

			{#if !loading && remoteStatus !== 'ok'}
				<li class="flex items-center justify-between gap-3 border-t border-base-300 p-3 text-sm">
					<span>
						{remoteStatus === 'timeout'
							? 'Suggestions took too long.'
							: 'Some suggestions are unavailable.'}
					</span>
					<Button size="xs" onclick={() => suggestQuery.refetch()}>Retry</Button>
				</li>
			{/if}
		</ul>
	{/if}
</div>
