<script lang="ts">
	import { onDestroy } from 'svelte';
	import { Check } from 'lucide-svelte';
	import type { GenrePrefItem, GenrePrefLevel } from '$lib/types';
	import {
		getGenrePrefsQuery,
		saveGenrePrefs
	} from '$lib/queries/genre-prefs/GenrePrefsQuery.svelte';
	import { toastStore } from '$lib/stores/toast';

	const prefsQuery = getGenrePrefsQuery();

	const LEVELS: { value: GenrePrefLevel; label: string }[] = [
		{ value: 'normal', label: 'Normal' },
		{ value: 'reduce', label: 'Reduce' },
		{ value: 'mute', label: 'Mute' }
	];

	// local working copy so level changes apply instantly; server state re-syncs it
	let genres = $state<GenrePrefItem[]>([]);
	let loaded = $state(false);
	let saveTimer: ReturnType<typeof setTimeout> | null = null;
	let saving = $state(false);
	let savedFlash = $state(false);
	let savedFlashTimer: ReturnType<typeof setTimeout> | null = null;

	$effect(() => {
		const serverGenres = prefsQuery.data?.genres;
		// adopt server state on first load, but never mid-edit
		if (serverGenres && !loaded && !saveTimer && !saving) {
			genres = serverGenres.map((g) => ({ ...g }));
			loaded = true;
		}
	});

	const adjustedCount = $derived(genres.filter((g) => g.level !== 'normal').length);

	function scheduleSave() {
		if (saveTimer) clearTimeout(saveTimer);
		saveTimer = setTimeout(() => void flushSave(), 600);
	}

	async function flushSave() {
		saveTimer = null;
		saving = true;
		try {
			await saveGenrePrefs({
				genres: genres.map((g) => ({ family: g.family, level: g.level }))
			});
			savedFlash = true;
			if (savedFlashTimer) clearTimeout(savedFlashTimer);
			savedFlashTimer = setTimeout(() => (savedFlash = false), 1500);
		} catch {
			toastStore.show({ message: "Couldn't save your genre balance", type: 'error' });
			// revert to server truth
			loaded = false;
			await prefsQuery.refetch();
		} finally {
			saving = false;
		}
	}

	function setLevel(family: string, level: GenrePrefLevel) {
		genres = genres.map((g) => (g.family === family ? { ...g, level } : g));
		scheduleSave();
	}

	onDestroy(() => {
		if (saveTimer) {
			clearTimeout(saveTimer);
			void flushSave();
		}
		if (savedFlashTimer) clearTimeout(savedFlashTimer);
	});
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<div class="flex items-start justify-between gap-3">
			<div>
				<h2 class="card-title text-2xl">Genre balance</h2>
				<p class="text-base-content/70">
					Keep one genre from dominating your recommendations. Reduce halves a genre's weight in
					Discover and the Taste Graph; Mute removes it entirely. Your library and listening history
					are unaffected.
				</p>
			</div>
			<div
				class="flex items-center gap-1.5 text-xs text-success transition-opacity duration-300"
				class:opacity-0={!savedFlash}
				aria-hidden={!savedFlash}
			>
				<Check class="h-3.5 w-3.5" />
				Saved
			</div>
		</div>

		{#if prefsQuery.isLoading && genres.length === 0}
			<div class="flex justify-center items-center py-12">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if prefsQuery.isError && genres.length === 0}
			<div class="alert alert-error mt-4">
				<span>Couldn't load your genre balance settings.</span>
				<button class="btn btn-sm" onclick={() => prefsQuery.refetch()}>Retry</button>
			</div>
		{:else if genres.length === 0}
			<p class="mt-4 text-sm text-base-content/50">
				No genres found yet - genres appear here once your library has been scanned.
			</p>
		{:else}
			<p class="px-1 pt-2 text-xs text-base-content/50">
				{adjustedCount === 0
					? 'All genres at normal weight'
					: `${adjustedCount} ${adjustedCount === 1 ? 'genre' : 'genres'} adjusted`}
			</p>
			<div class="mt-2 divide-y divide-base-content/5 rounded-xl bg-base-100/40">
				{#each genres as genre (genre.family)}
					<div class="flex items-center justify-between gap-4 px-4 py-3">
						<div class="min-w-0 flex-1">
							<span
								class="font-medium {genre.level === 'mute'
									? 'text-base-content/40 line-through'
									: ''}"
							>
								{genre.label}
							</span>
							{#if genre.artist_count > 0}
								<p class="text-xs text-base-content/50">
									{genre.artist_count}
									{genre.artist_count === 1 ? 'artist' : 'artists'} in your library
								</p>
							{/if}
						</div>
						<div
							class="join shrink-0"
							role="radiogroup"
							aria-label="Balance level for {genre.label}"
						>
							{#each LEVELS as option (option.value)}
								<button
									type="button"
									role="radio"
									aria-checked={genre.level === option.value}
									class="btn btn-xs join-item {genre.level === option.value
										? option.value === 'mute'
											? 'btn-error'
											: option.value === 'reduce'
												? 'btn-warning'
												: 'btn-primary'
										: 'btn-ghost bg-base-100/60'}"
									onclick={() => setLevel(genre.family, option.value)}
								>
									{option.label}
								</button>
							{/each}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
