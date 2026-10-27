<script lang="ts">
	import type { CrateTrack } from '$lib/types';
	import { getLibrarySearchV3Query } from '$lib/queries/local/LocalV3Queries.svelte';
	import {
		albumCoverUrl,
		crateCoverUrl,
		searchAlbumToSpin,
		searchTrackToCrateTrack,
		type SpinAlbum
	} from '$lib/queries/local/LocalV3Adapters';
	import { fly } from '$lib/ui/motion';
	import { Search, X, Disc3, Music2, Play, ListPlus, GripVertical, Loader } from 'lucide-svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';

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

	const albums = $derived((searchQuery.data?.albums ?? []).map(searchAlbumToSpin));
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

	function onAlbumDragStart(e: DragEvent, a: SpinAlbum) {
		if (!e.dataTransfer) return;
		e.dataTransfer.setData('application/x-crate-album', JSON.stringify(a));
		e.dataTransfer.effectAllowed = 'copy';
		draggingId = `album:${a.musicbrainz_id}`;
	}
</script>

{#snippet albumRowBody(a: SpinAlbum)}
	{@const cover = albumCoverUrl(a)}
	<div class="relative h-11 w-11 shrink-0 overflow-hidden rounded-md ring-1 ring-base-content/10">
		{#if cover}
			<AlbumImage
				customUrl={cover}
				alt={a.name}
				size="full"
				rounded="none"
				className="h-full w-full"
			/>
		{:else}
			<div class="relative h-full w-full overflow-hidden">
				<CoverPlaceholder id={a.musicbrainz_id} name={a.name} />
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
{/snippet}

<section class="flex h-full flex-col gap-3">
	<header class="flex items-center gap-2 px-1">
		<Search class="h-4 w-4 text-accent" />
		<h2 class="text-sm font-bold uppercase tracking-wider text-base-content/80">Search library</h2>
	</header>

	<div class="relative">
		<Input
			type="search"
			size="sm"
			icon={Search}
			label="Search your library for albums and songs"
			placeholder="Albums &amp; songs to spin…"
			class="w-full pr-9"
			bind:value={term}
		/>
		<div class="absolute right-1.5 top-1/2 flex -translate-y-1/2 items-center">
			{#if isActive && isFetching}
				<Loader
					class="mr-1 h-4 w-4 shrink-0 animate-spin text-base-content/40 motion-reduce:animate-none"
				/>
			{:else if term}
				<IconButton size="xs" icon={X} label="Clear search" onclick={() => (term = '')} />
			{/if}
		</div>
	</div>

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
						{@render albumRowBody(a)}
						<div
							class="flex shrink-0 flex-col gap-1 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100"
						>
							<IconButton size="xs" icon={Play} label="Play album" onclick={() => onPlayAlbum(a)} />
							<IconButton
								size="xs"
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
								<AlbumImage
									customUrl={trackCover}
									alt={t.album_name}
									size="full"
									rounded="none"
									className="h-full w-full"
								/>
							{:else}
								<div class="relative h-full w-full overflow-hidden">
									<CoverPlaceholder id={t.album_mbid} name={t.album_name} />
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
							class="flex shrink-0 flex-col gap-1 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100"
						>
							<IconButton size="xs" icon={Play} label="Play now" onclick={() => onPlayTrack(t)} />
							<IconButton
								size="xs"
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
	.search-row {
		transition:
			transform 0.2s var(--ease-spring),
			box-shadow 0.2s var(--ease-spring),
			border-color 0.2s ease;
	}

	.search-row:hover {
		transform: translateY(-2px);
		box-shadow: 0 12px 24px -14px oklch(from var(--color-base-100) l c h / 0.9);
	}

	.search-row.is-dragging {
		opacity: 0.4;
	}
</style>
