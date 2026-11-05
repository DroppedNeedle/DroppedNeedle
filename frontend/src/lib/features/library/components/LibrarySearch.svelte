<script lang="ts">
	import { Search, X, Music2, Play, ArrowUpRight } from 'lucide-svelte';
	import { fly, fade } from '$lib/ui/motion';
	import { getLibrarySearchQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import { buildDiscoveryQueueFromLocal } from '$lib/features/player/playback/queueHelpers';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import type { TrackView } from '$lib/features/library/queries/types';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import { withBasePath } from '$lib/utils/basePath';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';

	let term = $state('');
	let debounced = $state('');
	let focused = $state(false);
	let inputEl = $state<HTMLInputElement | null>(null);
	let rootEl = $state<HTMLElement>();

	// rotating placeholder hints signal what's searchable; first entry doubles as the resting label
	const HINTS = [
		'Search your library - artists, albums, tracks',
		"Try 'Radiohead'…",
		"Try 'In Rainbows'…",
		'Search a track…'
	];
	let hintIdx = $state(0);
	let reduced = $state(false);
	const hint = $derived(HINTS[hintIdx]);

	// debounce so each keystroke doesn't fire three endpoints
	$effect(() => {
		const next = term;
		const handle = setTimeout(() => (debounced = next.trim()), 180);
		return () => clearTimeout(handle);
	});

	// track motion preference so the hint rotation freezes when the user prefers no motion
	$effect(() => {
		const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
		reduced = mq.matches;
		const onChange = (e: MediaQueryListEvent) => (reduced = e.matches);
		mq.addEventListener('change', onChange);
		return () => mq.removeEventListener('change', onChange);
	});

	// cycle hints only while the bar is idle - pause once focused or typing so the rest text holds steady
	$effect(() => {
		if (reduced || focused || term) return;
		const id = setInterval(() => (hintIdx = (hintIdx + 1) % HINTS.length), 2800);
		return () => clearInterval(id);
	});

	const searchQuery = getLibrarySearchQuery(() => debounced);
	const results = $derived(searchQuery.data);
	const open = $derived(focused && debounced.length >= 2);
	const total = $derived(
		results ? results.artists.length + results.albums.length + results.tracks.length : 0
	);
	const showSkeleton = $derived(searchQuery.isLoading && !results);
	const discoverHref = $derived(withBasePath(`/search?q=${encodeURIComponent(term.trim())}`));

	function clear() {
		term = '';
		debounced = '';
		inputEl?.focus();
	}

	function onInputKeydown(e: KeyboardEvent) {
		if (e.key !== 'Escape') return;
		if (term) clear();
		else inputEl?.blur();
	}

	function playTrack(t: TrackView) {
		playerStore.playQueue(buildDiscoveryQueueFromLocal([t]), 0, false);
		focused = false;
	}

	// close the panel on pointer/focus outside the search root
	$effect(() => {
		if (!focused) return;
		const onAway = (e: Event) => {
			if (rootEl && !rootEl.contains(e.target as Node)) focused = false;
		};
		window.addEventListener('pointerdown', onAway, true);
		window.addEventListener('focusin', onAway, true);
		return () => {
			window.removeEventListener('pointerdown', onAway, true);
			window.removeEventListener('focusin', onAway, true);
		};
	});

	// "/" focuses the bar globally, unless already typing in a field
	$effect(() => {
		const onKey = (e: KeyboardEvent) => {
			if (e.key !== '/' || e.metaKey || e.ctrlKey || e.altKey) return;
			const el = document.activeElement;
			const typing =
				el instanceof HTMLElement &&
				(el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable);
			if (typing) return;
			e.preventDefault();
			inputEl?.focus();
		};
		window.addEventListener('keydown', onKey);
		return () => window.removeEventListener('keydown', onKey);
	});
</script>

<div class="relative z-30" bind:this={rootEl}>
	<div class="lib-search">
		<div class="relative flex items-center gap-3 px-3 py-2.5 sm:gap-5 sm:px-6 sm:py-3.5">
			<label
				for="library-search-input"
				class="flex min-w-0 flex-1 cursor-text items-center gap-4 sm:gap-6"
			>
				<span class="lib-search__well">
					<Search class="size-6 sm:size-7" />
				</span>
				<span class="min-w-0 flex-1">
					<span class="lib-search__line relative block">
						{#if !term}
							<span class="pointer-events-none absolute inset-0" aria-hidden="true">
								<!-- one hint fades out before the next fades in, so two never overlap -->
								{#key hintIdx}
									<span
										in:fade={{ duration: 300, delay: 240 }}
										out:fade={{ duration: 200 }}
										class="lib-search__hint absolute inset-0 flex items-center"
									>
										<span class="line-clamp-2">{hint}</span>
									</span>
								{/key}
							</span>
						{/if}
						<Input
							id="library-search-input"
							label="Search your library"
							borderless
							bind:ref={inputEl}
							bind:value={term}
							onfocus={() => (focused = true)}
							onkeydown={onInputKeydown}
							autocomplete="off"
							spellcheck="false"
							class="absolute inset-0 text-lg font-semibold text-base-content sm:text-xl"
						/>
					</span>
					<span class="lib-search__sub mt-0.5 block truncate">
						Find any artist, album or track instantly
					</span>
				</span>
			</label>
			{#if term}
				<IconButton label="Clear search" icon={X} class="shrink-0" onclick={clear} />
			{:else}
				<kbd class="lib-search__key">/</kbd>
			{/if}
		</div>
	</div>

	{#if open}
		<div
			transition:fly={{ y: -6, duration: 140 }}
			class="absolute inset-x-0 top-full z-40 mt-2 overflow-hidden rounded-2xl border border-base-content/10 bg-base-200/95 shadow-2xl backdrop-blur-xl"
		>
			{#if showSkeleton}
				<div class="space-y-2 p-3">
					{#each Array(4) as _, i (i)}
						<div class="flex items-center gap-3">
							<div class="skeleton h-10 w-10 shrink-0 rounded-lg"></div>
							<div class="flex-1 space-y-1.5">
								<div class="skeleton h-3 w-1/2 rounded"></div>
								<div class="skeleton h-2.5 w-1/3 rounded"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if total > 0 && results}
				<div class="max-h-[58vh] overflow-y-auto p-2">
					{#if results.artists.length}
						<div
							class="px-2 pt-2 pb-1 text-[11px] font-semibold tracking-wider text-muted uppercase"
						>
							Artists
						</div>
						{#each results.artists as artist (artist.id)}
							<a
								href={artistHref(artist.artist_mbid ?? artist.id)}
								class="flex items-center gap-3 rounded-xl px-2 py-2 transition-colors hover:bg-base-content/5"
							>
								<div class="h-10 w-10 shrink-0 overflow-hidden rounded-full">
									<ArtistImage
										mbid={artist.artist_mbid ?? ''}
										localId={artist.id}
										alt={artist.name}
										size="xs"
										className="h-full w-full object-cover"
									/>
								</div>
								<div class="min-w-0 flex-1">
									<div class="truncate text-sm font-semibold">{artist.name}</div>
									<div class="truncate text-xs text-muted">
										{artist.album_count}
										{artist.album_count === 1 ? 'album' : 'albums'} · {artist.led_track_count}
										{artist.led_track_count === 1 ? 'track' : 'tracks'}
									</div>
								</div>
								<ArrowUpRight class="h-4 w-4 shrink-0 text-base-content/30" />
							</a>
						{/each}
					{/if}

					{#if results.albums.length}
						<div
							class="px-2 pt-3 pb-1 text-[11px] font-semibold tracking-wider text-muted uppercase"
						>
							Albums
						</div>
						{#each results.albums as album (album.id)}
							<a
								href={albumHref(album.release_group_mbid ?? album.id)}
								class="flex items-center gap-3 rounded-xl px-2 py-2 transition-colors hover:bg-base-content/5"
							>
								<div class="h-10 w-10 shrink-0 overflow-hidden rounded-md">
									<AlbumImage
										albumId={album.id}
										mbid={album.release_group_mbid ?? ''}
										available={album.cover_available}
										alt={album.title}
										size="xs"
										requestSize={250}
										rounded="none"
										className="h-full w-full object-cover"
									/>
								</div>
								<div class="min-w-0 flex-1">
									<div class="truncate text-sm font-semibold">{album.title}</div>
									<div class="truncate text-xs text-muted">
										{album.year ?? 'Unknown'}{album.artist_name ? ` · ${album.artist_name}` : ''}
									</div>
								</div>
								<span class="shrink-0 text-xs text-muted">
									{album.track_count}
									{album.track_count === 1 ? 'track' : 'tracks'}
								</span>
							</a>
						{/each}
					{/if}

					{#if results.tracks.length}
						<div
							class="px-2 pt-3 pb-1 text-[11px] font-semibold tracking-wider text-muted uppercase"
						>
							Tracks
						</div>
						{#each results.tracks as track (track.id)}
							<button
								onclick={() => playTrack(track)}
								class="group/track flex w-full items-center gap-3 rounded-xl px-2 py-2 text-left transition-colors hover:bg-base-content/5"
							>
								<div class="relative h-10 w-10 shrink-0 overflow-hidden rounded-md">
									<AlbumImage
										albumId={track.album_id}
										mbid={track.release_group_mbid ?? ''}
										available={track.cover_available}
										alt={track.album_title}
										size="xs"
										requestSize={250}
										rounded="none"
										className="h-full w-full object-cover"
									/>
									<div
										class="absolute inset-0 flex items-center justify-center bg-base-100/50 opacity-0 backdrop-blur-[1px] transition-opacity group-hover/track:opacity-100"
									>
										<Play class="h-4 w-4 text-base-content" />
									</div>
								</div>
								<div class="min-w-0 flex-1">
									<div class="truncate text-sm font-semibold">{track.title}</div>
									<div class="truncate text-xs text-muted">
										{track.artist_name}{track.album_title ? ` · ${track.album_title}` : ''}
									</div>
								</div>
								<Music2 class="h-4 w-4 shrink-0 text-base-content/25" />
							</button>
						{/each}
					{/if}
				</div>
			{:else}
				<div class="px-4 py-6 text-center text-sm text-muted">
					No matches in your library for "<span class="font-medium text-base-content/80"
						>{debounced}</span
					>".
				</div>
			{/if}

			<a
				href={discoverHref}
				class="flex items-center justify-between gap-2 border-t border-base-content/10 bg-base-100/40 px-4 py-3 text-sm transition-colors hover:bg-base-100/70"
			>
				<span class="text-muted">Not in your library?</span>
				<span class="flex items-center gap-1.5 font-semibold text-primary">
					Search everywhere <ArrowUpRight class="h-4 w-4" />
				</span>
			</a>
		</div>
	{/if}
</div>

<style>
	.lib-search {
		position: relative;
		overflow: hidden;
		border: 1px solid color-mix(in oklab, var(--dn-title) 45%, transparent);
		border-radius: 0.875rem;
		background:
			radial-gradient(
				ellipse 70% 140% at 85% 50%,
				color-mix(in oklab, var(--dn-title) 22%, transparent),
				transparent 70%
			),
			linear-gradient(
				90deg,
				var(--color-base-100),
				color-mix(in oklab, var(--dn-title) 10%, var(--color-base-100))
			);
		transition:
			border-color 150ms,
			box-shadow 150ms;
	}
	.lib-search:focus-within {
		border-color: var(--dn-focus);
		box-shadow: 0 0 0 1px var(--dn-focus);
	}

	.lib-search__well {
		display: grid;
		flex: none;
		place-items: center;
		width: 4rem;
		height: 4rem;
		border: 1px solid var(--dn-line);
		border-radius: 0.625rem;
		background-color: var(--color-base-100);
		color: var(--color-base-content);
	}

	.lib-search__line {
		min-height: 2rem;
	}

	/* the hints in the header's Anton */
	.lib-search__hint {
		font-family: var(--font-display);
		font-size: 1.625rem;
		line-height: 1.05;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		color: var(--color-base-content);
		font-synthesis: none;
	}

	.lib-search__sub {
		font-size: 1rem;
		letter-spacing: 0.02em;
		color: var(--color-base-content);
		opacity: 0.85;
	}

	.lib-search__key {
		display: grid;
		flex: none;
		place-items: center;
		width: 2.25rem;
		height: 2.25rem;
		border: 1px solid var(--dn-edge);
		border-radius: 0.5rem;
		background-color: var(--color-base-100);
		font-family: var(--font-mono);
		font-size: 1rem;
		color: var(--color-base-content);
	}
	@media (max-width: 639px) {
		.lib-search__well {
			width: 2.5rem;
			height: 2.5rem;
		}
		.lib-search__line {
			min-height: 2.25rem;
		}
		.lib-search__hint {
			font-size: 1rem;
		}
		.lib-search__sub {
			font-size: 0.75rem;
		}
		.lib-search__key {
			width: 1.875rem;
			height: 1.875rem;
		}
	}
</style>
