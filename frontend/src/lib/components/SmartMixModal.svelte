<script lang="ts">
	/*
	 * Smart Mix — blend seeds into a real saved playlist. Add any mix of
	 * artists, genres, and moods to the basket; the backend builds the mix
	 * from your own files via the radio-plan engine, giving each seed a
	 * roughly equal share, and persists it as a native playlist.
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
	const MAX_SEEDS = 10;
	const NAME_SEED_LIMIT = 3;

	interface BlendSeed {
		type: SmartMixSeedType;
		value: string;
		label: string;
	}

	let dialogEl = $state<HTMLDialogElement | null>(null);
	let seedType = $state<SmartMixSeedType>('artist');
	let artistQuery = $state('');
	let customGenre = $state('');
	let blend = $state<BlendSeed[]>([]);
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
	const blendFull = $derived(blend.length >= MAX_SEEDS);
	const canCreate = $derived(blend.length > 0 && count !== null && !generateMutation.isPending);

	export function showModal() {
		errorMessage = null;
		dialogEl?.showModal();
	}

	function pickCustomCount() {
		countChoice = 'custom';
		queueMicrotask(() => customCountInputEl?.focus());
	}

	function inBlend(type: SmartMixSeedType, value: string): boolean {
		const v = value.trim().toLowerCase();
		return blend.some((s) => s.type === type && s.value.trim().toLowerCase() === v);
	}

	function addSeed(type: SmartMixSeedType, value: string, label: string) {
		const trimmed = value.trim();
		if (!trimmed || inBlend(type, trimmed)) return;
		if (blendFull) {
			toastStore.show({ message: `A blend holds up to ${MAX_SEEDS} seeds`, type: 'info' });
			return;
		}
		blend = [...blend, { type, value: trimmed, label }];
		errorMessage = null;
	}

	function removeSeed(seed: BlendSeed) {
		blend = blend.filter((s) => !(s.type === seed.type && s.value === seed.value));
	}

	function toggleSeed(type: SmartMixSeedType, value: string, label: string) {
		if (inBlend(type, value)) {
			const v = value.trim().toLowerCase();
			blend = blend.filter((s) => !(s.type === type && s.value.trim().toLowerCase() === v));
		} else {
			addSeed(type, value, label);
		}
	}

	function pickArtist(result: SuggestResult) {
		if (result.type !== 'artist') {
			toastStore.show({ message: 'Pick an artist result to seed the mix', type: 'info' });
			return;
		}
		addSeed('artist', result.musicbrainz_id, result.title);
		artistQuery = '';
	}

	function addCustomGenre() {
		const trimmed = customGenre.trim();
		if (!trimmed) return;
		addSeed('genre', trimmed, trimmed);
		customGenre = '';
	}

	// mirror the backend's default-name blend so the saved name uses the pretty
	// labels we have on hand (artist chips would otherwise fall back to MBIDs)
	const blendName = $derived.by(() => {
		const shown = blend.slice(0, NAME_SEED_LIMIT).map((s) => s.label);
		const rest = blend.length - shown.length;
		return `${shown.join(' + ')}${rest > 0 ? ` + ${rest} more` : ''} — Smart Mix`;
	});

	async function handleCreate() {
		if (!canCreate || count === null) return;
		errorMessage = null;
		try {
			const created = await generateMutation.mutateAsync({
				seeds: blend.map((s) => ({ type: s.type, value: s.value })),
				count,
				name: blendName
			});
			dialogEl?.close();
			blend = [];
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
			Blend artists, genres, and moods — we'll build a playlist from your library with every seed
			pulling its weight.
		</p>

		<div class="mt-5 space-y-5">
			<div>
				<p
					class="mb-2 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
				>
					Add seeds
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
							onclick={() => (seedType = t.key)}
						>
							{t.label}
						</button>
					{/each}
				</div>
			</div>

			{#if seedType === 'artist'}
				<div>
					<SearchSuggestions
						bind:query={artistQuery}
						onSearch={() => {}}
						onSelect={pickArtist}
						placeholder="Search for an artist to add..."
						id="smart-mix-artist"
					/>
				</div>
			{:else if seedType === 'genre'}
				<div class="space-y-2.5">
					{#if libraryGenres.length > 0}
						<div class="flex flex-wrap gap-2">
							{#each libraryGenres as genre (genre.name)}
								{@const active = inBlend('genre', genre.name)}
								<button
									type="button"
									class="rounded-full border px-3 py-1 text-xs font-medium capitalize transition-colors {active
										? 'border-primary/50 bg-primary text-primary-content'
										: 'border-base-content/10 bg-base-100/50 text-base-content/70 hover:border-primary/40 hover:text-base-content'}"
									aria-pressed={active}
									onclick={() => toggleSeed('genre', genre.name, genre.name)}
								>
									{genre.name}
								</button>
							{/each}
						</div>
					{/if}
					<input
						type="text"
						class="input input-sm w-full rounded-full"
						placeholder="Or type any genre and press Enter..."
						aria-label="Add a genre by name"
						bind:value={customGenre}
						onkeydown={(e) => e.key === 'Enter' && addCustomGenre()}
					/>
				</div>
			{:else}
				<div class="grid grid-cols-2 gap-2 sm:grid-cols-4">
					{#each MOODS as mood (mood)}
						{@const active = inBlend('mood', mood)}
						<button
							type="button"
							class="rounded-2xl border px-2 py-2.5 text-xs font-medium capitalize transition-colors {active
								? 'border-primary/50 bg-primary text-primary-content'
								: 'border-base-content/10 bg-base-100/50 text-base-content/70 hover:border-primary/40 hover:text-base-content'}"
							aria-pressed={active}
							onclick={() => toggleSeed('mood', mood, mood)}
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
					Your blend
					<span class="ml-1 font-mono tabular-nums text-base-content/35"
						>{blend.length}/{MAX_SEEDS}</span
					>
				</p>
				{#if blend.length === 0}
					<p
						class="rounded-2xl border border-dashed border-base-content/12 px-4 py-3 text-xs text-base-content/45"
					>
						Nothing yet — add at least one artist, genre, or mood above.
					</p>
				{:else}
					<div class="flex flex-wrap gap-2" role="list" aria-label="Selected seeds">
						{#each blend as seed (`${seed.type}:${seed.value}`)}
							<span
								role="listitem"
								class="flex items-center gap-1.5 rounded-full border border-primary/30 bg-primary/10 py-1 pl-3 pr-1 text-xs font-medium"
							>
								<span
									class="font-mono text-[0.55rem] font-bold uppercase tracking-[0.14em] text-base-content/45"
									>{seed.type}</span
								>
								<span class="capitalize">{seed.label}</span>
								<button
									type="button"
									class="btn btn-ghost btn-xs h-5 min-h-0 w-5 rounded-full p-0"
									aria-label="Remove {seed.label} from the blend"
									onclick={() => removeSeed(seed)}
								>
									<X class="h-3 w-3" />
								</button>
							</span>
						{/each}
					</div>
				{/if}
			</div>

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
