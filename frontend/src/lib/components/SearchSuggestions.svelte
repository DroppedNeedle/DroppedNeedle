<script lang="ts">
	import { getApiUrl } from '$lib/api/api-utils';
	import { Disc3, Search } from 'lucide-svelte';
	import type { SuggestResult } from '$lib/types';
	import { API } from '$lib/constants';
	import { isAbortError } from '$lib/utils/errorHandling';
	import { api } from '$lib/api/client';

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

	let suggestions = $state<SuggestResult[]>([]);
	let imageErrors = $state<Record<string, boolean>>({});
	let loading = $state(false);
	let showDropdown = $state(false);
	let activeIndex = $state(-1);
	let debounceTimeout: ReturnType<typeof setTimeout>;
	let abortController: AbortController | null = null;
	let rootRef: HTMLDivElement;
	let fetchGeneration = 0;

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
		abortController?.abort();
		abortController = null;
		activeIndex = -1;

		if (query.trim().length < 2) {
			suggestions = [];
			showDropdown = false;
			loading = false;
			return;
		}

		loading = true;
		showDropdown = true;

		debounceTimeout = setTimeout(async () => {
			abortController = new AbortController();
			const generation = ++fetchGeneration;

			try {
				const data = await api.get<{ results?: SuggestResult[] }>(
					API.search.suggest(query.trim(), 5),
					{
						signal: abortController.signal
					}
				);
				if (generation !== fetchGeneration) return;
				suggestions = data.results ?? [];
				// Clear stale cover-fetch errors: a cold cover now returns 202 (not a decodable
				// placeholder), so onerror fires and would otherwise pin the icon fallback for a
				// result that has since warmed and reappears in a later search.
				imageErrors = {};
				showDropdown = suggestions.length > 0 || loading;
			} catch (e) {
				if (isAbortError(e)) return;
				if (generation !== fetchGeneration) return;
				suggestions = [];
				showDropdown = false;
			} finally {
				if (generation === fetchGeneration) {
					loading = false;
				}
			}
		}, 600);
	}

	function handleSubmit(e: SubmitEvent) {
		e.preventDefault();
		showDropdown = false;
		suggestions = [];
		onSearch();
	}

	function handleSelect(result: SuggestResult) {
		showDropdown = false;
		suggestions = [];
		activeIndex = -1;
		onSelect(result);
	}

	function handleViewAll() {
		showDropdown = false;
		suggestions = [];
		activeIndex = -1;
		onSearch();
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			if (showDropdown) {
				e.preventDefault();
				e.stopPropagation();
				showDropdown = false;
				suggestions = [];
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
			abortController?.abort();
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

	{#if showDropdown && (suggestions.length > 0 || loading)}
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
							{#if imageErrors[result.musicbrainz_id]}
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
		</ul>
	{/if}
</div>
