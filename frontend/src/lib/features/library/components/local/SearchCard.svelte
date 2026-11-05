<script lang="ts">
	import '$lib/features/library/components/local/deck.css';
	import type { CrateTrack } from '$lib/features/library/queries/types';
	import { getLibrarySearchV3Query } from '$lib/features/library/queries/LocalV3Queries.svelte';
	import {
		albumCoverUrl,
		crateCoverUrl,
		searchAlbumToSpin,
		searchTrackToCrateTrack,
		type SpinAlbum
	} from '$lib/features/library/queries/LocalV3Adapters';
	import { fly } from '$lib/ui/motion';
	import { Search, X, Disc3, Music2, Play, ListPlus, GripVertical, Loader } from 'lucide-svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';

	interface Props {
		reducedMotion?: boolean;
		onPlayTrack: (t: CrateTrack) => void;
		onQueueTrack: (t: CrateTrack) => void;
		onPlayAlbum: (a: SpinAlbum) => void;
		onQueueAlbum: (a: SpinAlbum) => void;
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

	const searchQuery = getLibrarySearchV3Query(() => debounced);

	const albums = $derived(
		(searchQuery.data?.albums ?? []).flatMap((a) => searchAlbumToSpin(a) ?? [])
	);
	const tracks = $derived(
		(searchQuery.data?.tracks ?? []).flatMap((t) => searchTrackToCrateTrack(t) ?? [])
	);
	// Albums first, then tracks, each best match first.
	type Row =
		| { kind: 'album'; key: string; album: SpinAlbum }
		| { kind: 'track'; key: string; track: CrateTrack };
	const rows = $derived<Row[]>([
		...albums.map((a, i) => ({
			kind: 'album' as const,
			key: `album:${a.id}:${i}`,
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

	function onAlbumDragStart(e: DragEvent, a: SpinAlbum) {
		if (!e.dataTransfer) return;
		e.dataTransfer.setData('application/x-crate-album', JSON.stringify(a));
		e.dataTransfer.effectAllowed = 'copy';
		draggingId = `album:${a.id}`;
	}
</script>

<section class="lr-panel">
	<header class="lr-panel__head">
		<h2 class="lr-panel__title">Search library</h2>
	</header>

	<div class="lr-search relative px-0.5">
		<Input
			type="search"
			size="sm"
			icon={Search}
			label="Search your library for albums and songs"
			placeholder="Albums &amp; songs to spin…"
			class="w-full pr-9"
			bind:value={term}
		/>
		<div class="absolute right-2 top-1/2 flex -translate-y-1/2 items-center">
			{#if isActive && isFetching}
				<Loader class="mr-1 size-4 shrink-0 animate-spin text-muted motion-reduce:animate-none" />
			{:else if term}
				<IconButton size="xs" icon={X} label="Clear search" onclick={() => (term = '')} />
			{/if}
		</div>
	</div>

	<div class="min-h-0 flex-1 space-y-0.5 overflow-y-auto">
		{#if !isActive}
			<div class="search-idle">
				<Search class="size-6 text-muted" aria-hidden="true" />
				<p>Search your whole library - drag a song or a whole album onto the deck.</p>
			</div>
		{:else if rows.length === 0 && isFetching}
			{#each Array(4) as _, i (i)}
				<div class="flex items-center gap-2.5 px-1.5 py-[0.4375rem]">
					<div class="skeleton size-12 shrink-0 rounded"></div>
					<div class="flex-1 space-y-1.5">
						<div class="skeleton h-3.5 w-3/5"></div>
						<div class="skeleton h-3 w-2/5"></div>
					</div>
				</div>
			{/each}
		{:else if showEmpty}
			<div class="search-idle">
				<Disc3 class="size-6 text-muted" aria-hidden="true" />
				<p>No matches for <span class="font-semibold text-base-content">“{trimmed}”</span></p>
			</div>
		{:else}
			{#each rows as row (row.key)}
				{#if row.kind === 'album'}
					{@const a = row.album}
					{@const cover = albumCoverUrl(a)}
					<div
						class="lr-row"
						class:is-dragging={draggingId === row.key}
						draggable="true"
						role="button"
						tabindex="0"
						aria-label="{a.title} by {a.artist_name}, album"
						ondragstart={(e) => onAlbumDragStart(e, a)}
						ondragend={() => (draggingId = null)}
						ondblclick={() => onPlayAlbum(a)}
						onkeydown={(e) => {
							if (e.target !== e.currentTarget) return;
							if (e.key === 'Enter' || e.key === ' ') {
								e.preventDefault();
								onPlayAlbum(a);
							}
						}}
						in:fly={{ y: 10, duration: reducedMotion ? 0 : 200 }}
					>
						<GripVertical class="size-4 shrink-0 text-muted" aria-hidden="true" />
						<div class="lr-row__cover">
							{#if cover}
								<AlbumImage
									customUrl={cover}
									alt={a.title}
									size="full"
									rounded="none"
									className="h-full w-full"
								/>
							{:else}
								<CoverPlaceholder id={a.id} name={a.title} />
							{/if}
						</div>
						<div class="min-w-0 flex-1">
							<p class="lr-row__title">{a.title}</p>
							<p class="lr-row__sub">{a.artist_name}</p>
							<StatusChip size="xs" icon={Disc3} class="mt-1">Album</StatusChip>
						</div>
						<div class="lr-row__actions">
							<IconButton size="sm" icon={Play} label="Play album" onclick={() => onPlayAlbum(a)} />
							<IconButton
								size="sm"
								icon={ListPlus}
								label="Queue album"
								onclick={() => onQueueAlbum(a)}
							/>
						</div>
					</div>
				{:else}
					{@const t = row.track}
					{@const trackCover = crateCoverUrl(t)}
					<div
						class="lr-row"
						class:is-dragging={draggingId === row.key}
						draggable="true"
						role="button"
						tabindex="0"
						aria-label="{t.title} by {t.artist_name}, song"
						ondragstart={(e) => onTrackDragStart(e, t)}
						ondragend={() => (draggingId = null)}
						ondblclick={() => onPlayTrack(t)}
						onkeydown={(e) => {
							if (e.target !== e.currentTarget) return;
							if (e.key === 'Enter' || e.key === ' ') {
								e.preventDefault();
								onPlayTrack(t);
							}
						}}
						in:fly={{ y: 10, duration: reducedMotion ? 0 : 200 }}
					>
						<GripVertical class="size-4 shrink-0 text-muted" aria-hidden="true" />
						<div class="lr-row__cover">
							{#if trackCover}
								<AlbumImage
									customUrl={trackCover}
									alt={t.album_name}
									size="full"
									rounded="none"
									className="h-full w-full"
								/>
							{:else}
								<CoverPlaceholder id={t.album_id} name={t.album_name} />
							{/if}
						</div>
						<div class="min-w-0 flex-1">
							<p class="lr-row__title">{t.title}</p>
							<p class="lr-row__sub">{t.artist_name} · {t.album_name}</p>
							<StatusChip tone="action" size="xs" icon={Music2} class="mt-1">Song</StatusChip>
						</div>
						<div class="lr-row__actions">
							<IconButton size="sm" icon={Play} label="Play now" onclick={() => onPlayTrack(t)} />
							<IconButton
								size="sm"
								icon={ListPlus}
								label="Add to queue"
								onclick={() => onQueueTrack(t)}
							/>
						</div>
					</div>
				{/if}
			{/each}
		{/if}
	</div>
</section>

<style>
	/* the panel draws its own clear button */
	.lr-search :global(input::-webkit-search-cancel-button) {
		-webkit-appearance: none;
		display: none;
	}

	.search-idle {
		display: flex;
		height: 100%;
		min-height: 6rem;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		padding: 0.75rem 1.5rem;
		text-align: center;
		font-size: 0.8125rem;
		line-height: 1.45;
		color: var(--color-muted);
	}
</style>
