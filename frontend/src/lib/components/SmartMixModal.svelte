<script lang="ts">
	/*
	 * Smart Mix — one seed in, a real saved playlist out. Pick an artist, a
	 * genre from your library, or a mood; the backend builds the mix from your
	 * own files via the radio-plan engine and persists it as a native playlist.
	 */
	import { goto } from '$app/navigation';
	import { createGenerateSmartMixMutation } from '$lib/queries/playlists/PlaylistMutations.svelte';
	import { getDiscoverQuery } from '$lib/queries/discover/DiscoverQuery.svelte';
	import SearchSuggestions from '$lib/components/SearchSuggestions.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { SuggestResult, HomeGenre } from '$lib/types';
	import type { SmartMixSeedType } from '$lib/api/playlists';
	import { Sparkles, X } from 'lucide-svelte';

	const SEED_TYPES: { key: SmartMixSeedType; label: string }[] = [
		{ key: 'artist', label: 'Artist' },
		{ key: 'genre', label: 'Genre' },
		{ key: 'mood', label: 'Mood' }
	];

	// keep in sync with MOOD_TAG_FAMILIES in backend/services/smart_playlist_service.py
	const MOODS = [
		'chill',
		'energetic',
		'melancholy',
		'focus',
		'happy',
		'late night',
		'workout',
		'romantic'
	];

	const COUNTS = [15, 25, 50];
	const MIN_CUSTOM_COUNT = 1;
	const MAX_CUSTOM_COUNT = 250;

	let dialogEl = $state<HTMLDialogElement | null>(null);
	let seedType = $state<SmartMixSeedType>('artist');
	let artistQuery = $state('');
	let selectedArtist = $state<SuggestResult | null>(null);
	let selectedGenre = $state<string | null>(null);
	let customGenre = $state('');
	let selectedMood = $state<string | null>(null);
	let countChoice = $state<number | 'custom'>(25);
	let customCount = $state('');
	let customCountInputEl = $state<HTMLInputElement | null>(null);
	let errorMessage = $state<string | null>(null);

	const generateMutation = createGenerateSmartMixMutation();
	const discoverQuery = getDiscoverQuery();
	const libraryGenres = $derived(
		((discoverQuery.data?.genre_list?.items ?? []) as HomeGenre[])
			.filter((g) => !!g.name)
			.slice(0, 18)
	);

	const seed = $derived.by(() => {
		if (seedType === 'artist') return selectedArtist?.musicbrainz_id ?? '';
		if (seedType === 'genre') return selectedGenre ?? customGenre.trim();
		return selectedMood ?? '';
	});
	// custom count: only digits parse; anything else (or out of 1-250) is invalid
	const parsedCustomCount = $derived.by(() => {
		const trimmed = customCount.trim();
		if (!/^\d+$/.test(trimmed)) return null;
		const n = Number(trimmed);
		return n >= MIN_CUSTOM_COUNT && n <= MAX_CUSTOM_COUNT ? n : null;
	});
	const customCountInvalid = $derived(
		countChoice === 'custom' && customCount.trim().length > 0 && parsedCustomCount === null
	);
	const count = $derived(countChoice === 'custom' ? parsedCustomCount : countChoice);
	const canCreate = $derived(seed.length > 0 && count !== null && !generateMutation.isPending);

	function pickCustomCount() {
		countChoice = 'custom';
		queueMicrotask(() => customCountInputEl?.focus());
	}

	export function showModal() {
		errorMessage = null;
		dialogEl?.showModal();
	}

	function pickSeedType(type: SmartMixSeedType) {
		seedType = type;
		errorMessage = null;
	}

	function pickArtist(result: SuggestResult) {
		if (result.type !== 'artist') {
			toastStore.show({ message: 'Pick an artist result to seed the mix', type: 'info' });
			return;
		}
		selectedArtist = result;
		artistQuery = '';
	}

	async function handleCreate() {
		if (!canCreate || count === null) return;
		errorMessage = null;
		try {
			const created = await generateMutation.mutateAsync({
				seed_type: seedType,
				seed,
				count,
				...(seedType === 'artist' && selectedArtist
					? { name: `${selectedArtist.title} — Smart Mix` }
					: {})
			});
			dialogEl?.close();
			await goto(`/playlists/${created.id}`);
		} catch (e) {
			errorMessage = e instanceof Error ? e.message : "Couldn't create the Smart Mix";
		}
	}
</script>

<dialog bind:this={dialogEl} class="modal">
	<div class="modal-box max-w-lg rounded-2xl border border-base-content/10 bg-base-200">
		<p class="mb-1 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-accent">
			Auto playlist
		</p>
		<h3 class="flex items-center gap-2 font-display text-lg font-bold tracking-tight">
			<Sparkles class="h-4 w-4 text-accent" />
			Smart Mix
		</h3>
		<p class="mt-1 text-sm text-base-content/60">
			Seed it with an artist, genre, or mood — we'll build a playlist from your library.
		</p>

		<div class="mt-5 space-y-5">
			<div>
				<p
					class="mb-2 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
				>
					Seed
				</p>
				<div class="join w-full" role="tablist" aria-label="Seed type">
					{#each SEED_TYPES as t (t.key)}
						<button
							type="button"
							role="tab"
							aria-selected={seedType === t.key}
							class="join-item btn btn-sm flex-1 border-base-content/10"
							class:btn-primary={seedType === t.key}
							class:btn-ghost={seedType !== t.key}
							onclick={() => pickSeedType(t.key)}
						>
							{t.label}
						</button>
					{/each}
				</div>
			</div>

			{#if seedType === 'artist'}
				<div>
					{#if selectedArtist}
						<div
							class="flex items-center justify-between gap-2 rounded-2xl border border-base-content/8 bg-base-100/60 px-4 py-2.5"
						>
							<span class="truncate text-sm font-medium">{selectedArtist.title}</span>
							<button
								type="button"
								class="btn btn-ghost btn-xs rounded-full"
								aria-label="Clear selected artist"
								onclick={() => (selectedArtist = null)}
							>
								<X class="h-3.5 w-3.5" />
							</button>
						</div>
					{:else}
						<SearchSuggestions
							bind:query={artistQuery}
							onSearch={() => {}}
							onSelect={pickArtist}
							placeholder="Search for an artist..."
							id="smart-mix-artist"
						/>
					{/if}
				</div>
			{:else if seedType === 'genre'}
				<div>
					{#if libraryGenres.length > 0}
						<div class="flex flex-wrap gap-2">
							{#each libraryGenres as genre (genre.name)}
								<button
									type="button"
									class="rounded-full border px-3 py-1 text-xs font-medium capitalize transition-colors {selectedGenre ===
									genre.name
										? 'border-primary/50 bg-primary text-primary-content'
										: 'border-base-content/10 bg-base-100/50 text-base-content/70 hover:border-primary/40 hover:text-base-content'}"
									aria-pressed={selectedGenre === genre.name}
									onclick={() => (selectedGenre = selectedGenre === genre.name ? null : genre.name)}
								>
									{genre.name}
								</button>
							{/each}
						</div>
					{:else}
						<input
							type="text"
							class="input input-sm w-full rounded-full"
							placeholder="Type a genre, e.g. neo soul..."
							bind:value={customGenre}
							onkeydown={(e) => e.key === 'Enter' && void handleCreate()}
						/>
					{/if}
				</div>
			{:else}
				<div class="grid grid-cols-2 gap-2 sm:grid-cols-4">
					{#each MOODS as mood (mood)}
						<button
							type="button"
							class="rounded-2xl border px-2 py-2.5 text-xs font-medium capitalize transition-colors {selectedMood ===
							mood
								? 'border-primary/50 bg-primary text-primary-content'
								: 'border-base-content/10 bg-base-100/50 text-base-content/70 hover:border-primary/40 hover:text-base-content'}"
							aria-pressed={selectedMood === mood}
							onclick={() => (selectedMood = selectedMood === mood ? null : mood)}
						>
							{mood}
						</button>
					{/each}
				</div>
			{/if}

			<div>
				<p
					class="mb-2 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
				>
					Tracks
				</p>
				<div class="flex flex-wrap items-center gap-2">
					<div class="join" role="radiogroup" aria-label="Track count">
						{#each COUNTS as c (c)}
							<button
								type="button"
								role="radio"
								aria-checked={countChoice === c}
								class="join-item btn btn-sm border-base-content/10 font-mono tabular-nums"
								class:btn-primary={countChoice === c}
								class:btn-ghost={countChoice !== c}
								onclick={() => (countChoice = c)}
							>
								{c}
							</button>
						{/each}
						<button
							type="button"
							role="radio"
							aria-checked={countChoice === 'custom'}
							class="join-item btn btn-sm border-base-content/10"
							class:btn-primary={countChoice === 'custom'}
							class:btn-ghost={countChoice !== 'custom'}
							onclick={pickCustomCount}
						>
							Custom
						</button>
					</div>
					{#if countChoice === 'custom'}
						<input
							type="text"
							inputmode="numeric"
							class="input input-sm w-24 rounded-full font-mono tabular-nums"
							class:input-error={customCountInvalid}
							placeholder="1–250"
							aria-label="Custom track count, {MIN_CUSTOM_COUNT} to {MAX_CUSTOM_COUNT}"
							aria-invalid={customCountInvalid}
							bind:this={customCountInputEl}
							bind:value={customCount}
							onkeydown={(e) => e.key === 'Enter' && void handleCreate()}
						/>
					{/if}
				</div>
				{#if customCountInvalid}
					<p class="mt-1.5 text-xs text-error" role="status">
						Enter a whole number between {MIN_CUSTOM_COUNT} and {MAX_CUSTOM_COUNT}.
					</p>
				{/if}
			</div>

			{#if errorMessage}
				<div role="alert" class="alert alert-error py-2 text-sm">
					<span>{errorMessage}</span>
				</div>
			{/if}
		</div>

		<div class="modal-action">
			<form method="dialog">
				<button class="btn btn-ghost rounded-full bg-base-content/6">Cancel</button>
			</form>
			<button
				class="btn btn-primary rounded-full"
				onclick={() => void handleCreate()}
				disabled={!canCreate}
			>
				{#if generateMutation.isPending}
					<span class="loading loading-spinner loading-xs"></span>
				{:else}
					<Sparkles class="h-4 w-4" />
				{/if}
				Create Smart Mix
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>close</button>
	</form>
</dialog>
