<script lang="ts">
	import { getApiUrl } from '$lib/api/api-utils';
	import { Disc3, Search } from 'lucide-svelte';
	import type { SearchRemoteStatus, SuggestResult } from '$lib/types';
	import { getSearchSuggestionsV3Query } from '$lib/queries/search/SearchV3Queries.svelte';
	import {
		toSearchRemoteStatus,
		toSuggestResultsV1
	} from '$lib/queries/search/SearchV3Adapters';

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

	let imageErrors = $state<Record<string, boolean>>({});
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

	function coverUrl(result: SuggestResult): string {
		return result.type === 'artist'
			? getApiUrl(`/api/v1/covers/artist/${result.musicbrainz_id}?size=250`)
			: getApiUrl(`/api/v1/covers/release-group/${result.musicbrainz_id}?size=250`);
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
			imageErrors = {};
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
		<label class="input input-bordered flex items-center gap-2 w-full {inputClass}">
			<Search class="h-[1em] opacity-50" strokeWidth={2.5} />
			<!-- svelte-ignore a11y_autofocus -->
			<input
				type="search"
				{placeholder}
				bind:value={query}
				oninput={handleInput}
				onkeydown={handleKeydown}
				class="grow"
				autocomplete="off"
				aria-autocomplete="list"
				aria-controls={listboxId}
				aria-activedescendant={activeDescendant}
				{autofocus}
			/>
			{#if loading}
				<span class="loading loading-spinner loading-sm"></span>
			{/if}
		</label>
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
					<div class="avatar avatar-placeholder">
						<div class="w-10 h-10 rounded bg-base-200 flex items-center justify-center">
							{#if (result.local_id && result.local_id === result.musicbrainz_id) || imageErrors[result.musicbrainz_id]}
								<Disc3 class="h-5 w-5 text-base-content/20" />
							{:else}
								<img
									src={coverUrl(result)}
									alt={result.title}
									class="w-full h-full object-cover rounded"
									onerror={() => {
										imageErrors[result.musicbrainz_id] = true;
									}}
								/>
							{/if}
						</div>
					</div>
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
					<button class="btn btn-xs" onclick={() => suggestQuery.refetch()}>Retry</button>
				</li>
			{/if}
		</ul>
	{/if}
</div>
