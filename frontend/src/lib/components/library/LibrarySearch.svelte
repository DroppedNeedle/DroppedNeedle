<script lang="ts">
	import { Search, X, Users, Music2, Play, ArrowUpRight } from 'lucide-svelte';
	import { fly, fade } from 'svelte/transition';
	import { getLibrarySearchQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import { isValidMbid } from '$lib/utils/formatting';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import { buildDiscoveryQueueFromLocal } from '$lib/player/queueHelpers';
	import { playerStore } from '$lib/stores/player.svelte';
	import type { NativeTrackListItem } from '$lib/types';

	let term = $state('');
	let debounced = $state('');
	let focused = $state(false);
	let inputEl = $state<HTMLInputElement>();
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
	const discoverHref = $derived(`/search?q=${encodeURIComponent(term.trim())}`);

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

	function playTrack(t: NativeTrackListItem) {
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
	<div
		class="group/bar relative overflow-hidden rounded-3xl border border-base-content/10 bg-gradient-to-br from-primary/12 via-base-200/40 to-accent/10 shadow-lg backdrop-blur-sm transition-all duration-300 focus-within:border-primary/45 focus-within:shadow-[0_0_60px_-14px_oklch(from_var(--color-primary)_l_c_h/0.55)]"
	>
		<div
			aria-hidden="true"
			class="pointer-events-none absolute -top-16 -right-12 h-48 w-48 rounded-full bg-primary/20 blur-3xl transition-transform duration-500 group-focus-within/bar:scale-125"
		></div>
		<div
			aria-hidden="true"
			class="pointer-events-none absolute inset-0 opacity-[0.03]"
			style="background-image:url('data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 200 200%22><filter id=%22n%22><feTurbulence type=%22fractalNoise%22 baseFrequency=%220.9%22 numOctaves=%224%22 stitchTiles=%22stitch%22/></filter><rect width=%22100%25%22 height=%22100%25%22 filter=%22url(%23n)%22 opacity=%220.5%22/></svg>');background-size:200px;"
		></div>

		<div class="relative flex items-center gap-4 px-5 py-5 sm:gap-5 sm:px-7 sm:py-6">
			<label
				for="library-search-input"
				class="flex min-w-0 flex-1 cursor-text items-center gap-4 sm:gap-5"
			>
				<span
					class="grid h-12 w-12 shrink-0 place-items-center rounded-2xl bg-base-100/50 text-base-content/50 ring-1 ring-base-content/10 transition-colors group-focus-within/bar:bg-primary/15 group-focus-within/bar:text-primary group-focus-within/bar:ring-primary/30 sm:h-14 sm:w-14"
				>
					<Search class="h-5 w-5 sm:h-6 sm:w-6" />
				</span>
				<span class="min-w-0 flex-1">
					<span class="relative block h-7 sm:h-8">
						{#if !term}
							<span class="pointer-events-none absolute inset-0" aria-hidden="true">
								{#key hintIdx}
									<span
										in:fade={{ duration: 420 }}
										out:fade={{ duration: 320 }}
										class="absolute inset-0 flex items-center text-lg font-semibold text-base-content/40 sm:text-xl"
									>
										<span class="truncate">{hint}</span>
									</span>
								{/key}
							</span>
						{/if}
						<input
							id="library-search-input"
							bind:this={inputEl}
							bind:value={term}
							onfocus={() => (focused = true)}
							onkeydown={onInputKeydown}
							type="text"
							placeholder=""
							aria-label="Search your library"
							autocomplete="off"
							spellcheck="false"
							class="absolute inset-0 w-full bg-transparent text-lg font-semibold text-base-content outline-none sm:text-xl"
						/>
					</span>
					<span class="mt-1 block truncate text-sm text-base-content/55">
						Find any artist, album or track instantly
					</span>
				</span>
			</label>
			{#if term}
				<button onclick={clear} class="btn btn-circle btn-ghost shrink-0" aria-label="Clear search">
					<X class="h-5 w-5" />
				</button>
			{:else}
				<kbd
					class="hidden shrink-0 self-start rounded-lg border border-base-content/15 bg-base-100/50 px-2 py-1 text-sm font-medium text-base-content/45 sm:inline-block"
					>/</kbd
				>
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
							class="px-2 pt-2 pb-1 text-[11px] font-semibold tracking-wider text-base-content/40 uppercase"
						>
							Artists
						</div>
						{#each results.artists as artist (artist.artist_name + (artist.artist_mbid ?? ''))}
							{@const artistLinkable = isValidMbid(artist.artist_mbid)}
							<svelte:element
								this={artistLinkable ? 'a' : 'div'}
								href={artistLinkable && artist.artist_mbid
									? artistHref(artist.artist_mbid)
									: undefined}
								class="flex items-center gap-3 rounded-xl px-2 py-2 transition-colors hover:bg-base-content/5 {artistLinkable
									? ''
									: 'cursor-default opacity-80'}"
							>
								{#if artistLinkable}
									<div class="h-10 w-10 shrink-0 overflow-hidden rounded-full">
										<ArtistImage
											mbid={artist.artist_mbid ?? ''}
											alt={artist.artist_name}
											size="xs"
											className="h-full w-full object-cover"
										/>
									</div>
								{:else}
									<div
										class="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-accent/15 text-accent ring-1 ring-accent/25"
									>
										<Users class="h-5 w-5" />
									</div>
								{/if}
								<div class="min-w-0 flex-1">
									<div class="truncate text-sm font-semibold">{artist.artist_name}</div>
									<div class="truncate text-xs text-base-content/50">
										{artist.album_count}
										{artist.album_count === 1 ? 'album' : 'albums'} · {artist.track_count}
										{artist.track_count === 1 ? 'track' : 'tracks'}
									</div>
								</div>
								{#if artistLinkable}
									<ArrowUpRight class="h-4 w-4 shrink-0 text-base-content/30" />
								{/if}
							</svelte:element>
						{/each}
					{/if}

					{#if results.albums.length}
						<div
							class="px-2 pt-3 pb-1 text-[11px] font-semibold tracking-wider text-base-content/40 uppercase"
						>
							Albums
						</div>
						{#each results.albums as album (album.release_group_mbid)}
							<a
								href={albumHref(album.release_group_mbid)}
								class="flex items-center gap-3 rounded-xl px-2 py-2 transition-colors hover:bg-base-content/5"
							>
								<div class="h-10 w-10 shrink-0 overflow-hidden rounded-md">
									<AlbumImage
										mbid={album.release_group_mbid}
										remoteUrl={album.cover_url}
										alt={album.album_title}
										size="xs"
										rounded="none"
										className="h-full w-full object-cover"
									/>
								</div>
								<div class="min-w-0 flex-1">
									<div class="truncate text-sm font-semibold">{album.album_title}</div>
									<div class="truncate text-xs text-base-content/50">
										{album.year ?? 'Unknown'}{album.album_artist_name
											? ` · ${album.album_artist_name}`
											: ''}
									</div>
								</div>
								<span class="shrink-0 text-xs text-base-content/40">
									{album.track_count}
									{album.track_count === 1 ? 'track' : 'tracks'}
								</span>
							</a>
						{/each}
					{/if}

					{#if results.tracks.length}
						<div
							class="px-2 pt-3 pb-1 text-[11px] font-semibold tracking-wider text-base-content/40 uppercase"
						>
							Tracks
						</div>
						{#each results.tracks as track (track.track_file_id)}
							<button
								onclick={() => playTrack(track)}
								class="group/track flex w-full items-center gap-3 rounded-xl px-2 py-2 text-left transition-colors hover:bg-base-content/5"
							>
								<div class="relative h-10 w-10 shrink-0 overflow-hidden rounded-md">
									<AlbumImage
										mbid={track.album_mbid ?? ''}
										remoteUrl={track.cover_url}
										alt={track.album_name}
										size="xs"
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
									<div class="truncate text-xs text-base-content/50">
										{track.artist_name}{track.album_name ? ` · ${track.album_name}` : ''}
									</div>
								</div>
								<Music2 class="h-4 w-4 shrink-0 text-base-content/25" />
							</button>
						{/each}
					{/if}
				</div>
			{:else}
				<div class="px-4 py-6 text-center text-sm text-base-content/55">
					No matches in your library for "<span class="font-medium text-base-content/80"
						>{debounced}</span
					>".
				</div>
			{/if}

			<a
				href={discoverHref}
				class="flex items-center justify-between gap-2 border-t border-base-content/10 bg-base-100/40 px-4 py-3 text-sm transition-colors hover:bg-base-100/70"
			>
				<span class="text-base-content/60">Not in your library?</span>
				<span class="flex items-center gap-1.5 font-semibold text-primary">
					Search everywhere <ArrowUpRight class="h-4 w-4" />
				</span>
			</a>
		</div>
	{/if}
</div>
