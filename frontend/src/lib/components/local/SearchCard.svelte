<script lang="ts">
	import { SvelteMap } from 'svelte/reactivity';
	import type { CrateTrack, LocalAlbumSummary } from '$lib/types';
	import { getLocalSearchV3Query } from '$lib/queries/local/LocalV3Queries.svelte';
	import {
		albumCardToSummary,
		albumCoverUrl,
		crateCoverUrl,
		suggestionToCrateTrack
	} from '$lib/queries/local/LocalV3Adapters';
	import { fly } from 'svelte/transition';
	import { Search, X, Disc3, Music2, Play, ListPlus, GripVertical, Loader } from 'lucide-svelte';

	interface Props {
		reducedMotion?: boolean;
		onPlayTrack: (t: CrateTrack) => void;
		onQueueTrack: (t: CrateTrack) => void;
		onPlayAlbum: (a: LocalAlbumSummary) => void;
		onQueueAlbum: (a: LocalAlbumSummary) => void;
	}

	let {
		reducedMotion = false,
		onPlayTrack,
		onQueueTrack,
		onPlayAlbum,
		onQueueAlbum
	}: Props = $props();

	let term = $state('');
	let debounced = $state('');
	let draggingId = $state<string | null>(null);

	// Debounce keystrokes so we don't fire a request per character.
	$effect(() => {
		const next = term;
		const id = setTimeout(() => (debounced = next), 220);
		return () => clearTimeout(id);
	});

	const searchQuery = getLocalSearchV3Query(() => debounced);

	const albums = $derived((searchQuery.data?.albums ?? []).map(albumCardToSummary));
	// Track covers resolve through the same response's album cards.
	const tracks = $derived.by(() => {
		const mbidByAlbumId = new SvelteMap(
			(searchQuery.data?.albums ?? []).map((a) => [a.id, a.release_group_mbid ?? null] as const)
		);
		return (searchQuery.data?.tracks ?? []).map((t) =>
			suggestionToCrateTrack(t, mbidByAlbumId.get(t.album_id) ?? null)
		);
	});
	// Albums first, then tracks in backend order.
	type Row =
		| { kind: 'album'; key: string; album: LocalAlbumSummary }
		| { kind: 'track'; key: string; track: CrateTrack };
	const rows = $derived<Row[]>([
		...albums.map((a, i) => ({
			kind: 'album' as const,
			key: `album:${a.musicbrainz_id}:${i}`,
			album: a
		})),
		...tracks.map((t) => ({ kind: 'track' as const, key: `track:${t.track_file_id}`, track: t }))
	]);

	const trimmed = $derived(term.trim());
	const isActive = $derived(trimmed.length >= 2);
	const isFetching = $derived(searchQuery.isFetching);
	const showEmpty = $derived(isActive && !isFetching && rows.length === 0);

	function onTrackDragStart(e: DragEvent, t: CrateTrack) {
		if (!e.dataTransfer) return;
		e.dataTransfer.setData('application/x-crate-track', JSON.stringify(t));
		e.dataTransfer.effectAllowed = 'copy';
		draggingId = `track:${t.track_file_id}`;
	}

	function onAlbumDragStart(e: DragEvent, a: LocalAlbumSummary) {
		if (!e.dataTransfer) return;
		e.dataTransfer.setData('application/x-crate-album', JSON.stringify(a));
		e.dataTransfer.effectAllowed = 'copy';
		draggingId = `album:${a.musicbrainz_id}`;
	}
</script>

<section class="flex h-full flex-col gap-3">
	<header class="flex items-center gap-2 px-1">
		<Search class="h-4 w-4 text-accent" />
		<h2 class="text-sm font-bold uppercase tracking-wider text-base-content/80">Search library</h2>
	</header>

	<label
		class="input input-sm flex items-center gap-2 rounded-xl bg-base-200/70 focus-within:outline-accent"
	>
		<Search class="h-4 w-4 shrink-0 text-base-content/40" />
		<input
			type="search"
			class="grow bg-transparent"
			placeholder="Albums &amp; songs to spin…"
			bind:value={term}
			aria-label="Search your library for albums and songs"
		/>
		{#if isActive && isFetching}
			<Loader class="h-4 w-4 shrink-0 animate-spin text-base-content/40" />
		{:else if term}
			<button
				class="btn btn-circle btn-ghost btn-xs"
				onclick={() => (term = '')}
				aria-label="Clear search"
				title="Clear"
			>
				<X class="h-3.5 w-3.5" />
			</button>
		{/if}
	</label>

	<div class="flex-1 space-y-2 overflow-y-auto pr-0.5">
		{#if !isActive}
			<div
				class="flex h-full flex-col items-center justify-center gap-2 px-4 text-center text-base-content/40"
			>
				<Search class="h-8 w-8 opacity-40" />
				<p class="text-xs">
					Search your whole library - drag a song or a whole album onto the deck.
				</p>
			</div>
		{:else if rows.length === 0 && isFetching}
			{#each Array(4) as _, i (i)}
				<div class="h-[3.75rem] animate-pulse rounded-xl bg-base-200/60"></div>
			{/each}
		{:else if showEmpty}
			<div
				class="flex h-full flex-col items-center justify-center gap-2 px-4 text-center text-base-content/45"
			>
				<Disc3 class="h-8 w-8 opacity-40" />
				<p class="text-xs">
					No matches for <span class="font-semibold text-base-content/70">“{trimmed}”</span>
				</p>
			</div>
		{:else}
			{#each rows as row (row.key)}
				{#if row.kind === 'album'}
					{@const a = row.album}
					{@const cover = albumCoverUrl(a)}
					<div
						class="search-row group flex items-center gap-3 rounded-xl border border-base-content/5 bg-base-200/70 p-2 backdrop-blur-sm"
						class:is-dragging={draggingId === row.key}
						draggable="true"
						role="button"
						tabindex="0"
						ondragstart={(e) => onAlbumDragStart(e, a)}
						ondragend={() => (draggingId = null)}
						ondblclick={() => onPlayAlbum(a)}
						onkeydown={(e) => {
							if (e.key === 'Enter' || e.key === ' ') {
								e.preventDefault();
								onPlayAlbum(a);
							}
						}}
						in:fly={{ y: 10, duration: reducedMotion ? 0 : 200 }}
					>
						<GripVertical
							class="h-4 w-4 shrink-0 cursor-grab text-base-content/25 group-hover:text-base-content/50"
						/>
						<div
							class="relative h-11 w-11 shrink-0 overflow-hidden rounded-md ring-1 ring-base-content/10"
						>
							{#if cover}
								<img src={cover} alt={a.name} class="h-full w-full object-cover" loading="lazy" />
							{:else}
								<div class="flex h-full w-full items-center justify-center bg-base-300">
									<Disc3 class="h-4 w-4 text-base-content/30" />
								</div>
							{/if}
						</div>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm font-semibold text-base-content">{a.name}</p>
							<p class="truncate text-xs text-base-content/55">{a.artist_name}</p>
							<span class="badge badge-xs badge-primary mt-1 gap-1 border-none">
								<Disc3 class="h-2.5 w-2.5" /> Album
							</span>
						</div>
						<div
							class="flex shrink-0 flex-col gap-1 opacity-0 transition-opacity group-hover:opacity-100"
						>
							<button
								class="btn btn-circle btn-ghost btn-xs"
								onclick={() => onPlayAlbum(a)}
								aria-label="Play album"
								title="Play album"
							>
								<Play class="h-3.5 w-3.5" />
							</button>
							<button
								class="btn btn-circle btn-ghost btn-xs"
								onclick={() => onQueueAlbum(a)}
								aria-label="Queue album"
								title="Queue album"
							>
								<ListPlus class="h-3.5 w-3.5" />
							</button>
						</div>
					</div>
				{:else}
					{@const t = row.track}
					{@const trackCover = crateCoverUrl(t)}
					<div
						class="search-row group flex items-center gap-3 rounded-xl border border-base-content/5 bg-base-200/70 p-2 backdrop-blur-sm"
						class:is-dragging={draggingId === row.key}
						draggable="true"
						role="button"
						tabindex="0"
						ondragstart={(e) => onTrackDragStart(e, t)}
						ondragend={() => (draggingId = null)}
						ondblclick={() => onPlayTrack(t)}
						onkeydown={(e) => {
							if (e.key === 'Enter' || e.key === ' ') {
								e.preventDefault();
								onPlayTrack(t);
							}
						}}
						in:fly={{ y: 10, duration: reducedMotion ? 0 : 200 }}
					>
						<GripVertical
							class="h-4 w-4 shrink-0 cursor-grab text-base-content/25 group-hover:text-base-content/50"
						/>
						<div
							class="relative h-11 w-11 shrink-0 overflow-hidden rounded-md ring-1 ring-base-content/10"
						>
							{#if trackCover}
								<img
									src={trackCover}
									alt={t.album_name}
									class="h-full w-full object-cover"
									loading="lazy"
								/>
							{:else}
								<div class="flex h-full w-full items-center justify-center bg-base-300">
									<Music2 class="h-4 w-4 text-base-content/30" />
								</div>
							{/if}
						</div>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm font-semibold text-base-content">{t.title}</p>
							<p class="truncate text-xs text-base-content/55">
								{t.artist_name}<span class="text-base-content/35"> · {t.album_name}</span>
							</p>
							<span class="badge badge-xs badge-accent mt-1 gap-1 border-none">
								<Music2 class="h-2.5 w-2.5" /> Song
							</span>
						</div>
						<div
							class="flex shrink-0 flex-col gap-1 opacity-0 transition-opacity group-hover:opacity-100"
						>
							<button
								class="btn btn-circle btn-ghost btn-xs"
								onclick={() => onPlayTrack(t)}
								aria-label="Play now"
								title="Play now"
							>
								<Play class="h-3.5 w-3.5" />
							</button>
							<button
								class="btn btn-circle btn-ghost btn-xs"
								onclick={() => onQueueTrack(t)}
								aria-label="Add to queue"
								title="Add to queue"
							>
								<ListPlus class="h-3.5 w-3.5" />
							</button>
						</div>
					</div>
				{/if}
			{/each}
		{/if}
	</div>
</section>
