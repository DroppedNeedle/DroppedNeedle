<script lang="ts">
	/*
	 * CommandPalette — the global search + command surface (Ctrl/Cmd-K or "/").
	 *
	 * Replaces the old plain search modal while keeping its contract: the dialog
	 * keeps the #search_modal id so every existing showModal() caller still
	 * works, music suggestions hit the same suggest API, Enter falls through to
	 * the full /search page. On top of that it adds navigation jumps and
	 * permission-gated quick actions.
	 */
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { api } from '$lib/api/client';
	import { API } from '$lib/constants';
	import { getApiUrl } from '$lib/api/api-utils';
	import { isAbortError } from '$lib/utils/errorHandling';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { appearance, THEME_OPTIONS } from '$lib/stores/appearance.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { playerUi } from '$lib/stores/playerUi.svelte';
	import { launchRadio } from '$lib/player/launchRadio';
	import ShortcutsHelp from '$lib/shell/ShortcutsHelp.svelte';
	import { fromStore } from 'svelte/store';
	import type { SuggestResult } from '$lib/types';
	import {
		Search,
		Disc3,
		House,
		Compass,
		Library,
		ListMusic,
		Heart,
		Download,
		Inbox,
		Settings,
		UserRound,
		Headphones,
		Palette,
		Maximize2,
		CornerDownLeft,
		History,
		Play
	} from 'lucide-svelte';

	const integrations = fromStore(integrationStore);

	let dialogEl: HTMLDialogElement | undefined = $state();
	let inputEl: HTMLInputElement | undefined = $state();
	let query = $state('');
	let suggestions = $state<SuggestResult[]>([]);
	let loading = $state(false);
	let activeIndex = $state(0);
	let imageErrors = $state<Record<string, boolean>>({});
	let debounceTimeout: ReturnType<typeof setTimeout>;
	let abortController: AbortController | null = null;
	let fetchGeneration = 0;

	const ytConfigured = $derived(!!integrations.current.youtube_api);

	/* ---- recent searches (persisted, newest first, deduped) ---- */
	type RecentEntry =
		| { kind: 'suggestion'; title: string; type: 'artist' | 'album'; mbid: string }
		| { kind: 'query'; query: string };

	const RECENT_KEY = 'dn-recent-searches';
	const RECENT_MAX = 6;

	let recents = $state<RecentEntry[]>([]);

	function isRecentEntry(v: unknown): v is RecentEntry {
		if (!v || typeof v !== 'object') return false;
		const r = v as Record<string, unknown>;
		if (r.kind === 'query') return typeof r.query === 'string' && r.query.length > 0;
		return (
			r.kind === 'suggestion' &&
			typeof r.title === 'string' &&
			typeof r.mbid === 'string' &&
			(r.type === 'artist' || r.type === 'album')
		);
	}

	function loadRecents() {
		try {
			const raw = localStorage.getItem(RECENT_KEY);
			const parsed: unknown = raw ? JSON.parse(raw) : [];
			recents = Array.isArray(parsed) ? parsed.filter(isRecentEntry).slice(0, RECENT_MAX) : [];
		} catch {
			recents = [];
		}
	}

	function recentKeyOf(entry: RecentEntry): string {
		return entry.kind === 'suggestion' ? `s:${entry.mbid}` : `q:${entry.query.toLowerCase()}`;
	}

	function recordRecent(entry: RecentEntry) {
		const key = recentKeyOf(entry);
		recents = [entry, ...recents.filter((r) => recentKeyOf(r) !== key)].slice(0, RECENT_MAX);
		try {
			localStorage.setItem(RECENT_KEY, JSON.stringify(recents));
		} catch {
			// storage unavailable - recents just won't persist
		}
	}

	function clearRecents() {
		recents = [];
		try {
			localStorage.removeItem(RECENT_KEY);
		} catch {
			// ignore
		}
		activeIndex = 0;
		inputEl?.focus();
	}

	// recents only surface while the input is empty
	const visibleRecents = $derived(query.trim() ? [] : recents);

	function activateRecent(entry: RecentEntry) {
		recordRecent(entry); // bump to top
		close();
		if (entry.kind === 'suggestion') {
			const routeId = entry.type === 'artist' ? '/artist/[id]' : '/album/[id]';
			goto(resolve(routeId, { id: entry.mbid }));
		} else {
			goto(`/search?q=${encodeURIComponent(entry.query)}`);
		}
	}

	type Command = {
		id: string;
		label: string;
		hint?: string;
		icon: typeof House;
		keywords?: string;
		run: () => void;
	};

	function navCommand(
		id: string,
		label: string,
		icon: typeof House,
		path: string,
		keywords = ''
	): Command {
		return { id, label, icon, keywords, hint: 'Go to', run: () => goto(path) };
	}

	const allCommands = $derived.by(() => {
		const downloadClientConfigured =
			integrations.current.download_client || !integrations.current.loaded;
		const cmds: Command[] = [
			navCommand('home', 'Home', House, '/'),
			navCommand('discover', 'Discover', Compass, '/discover', 'recommendations'),
			navCommand('library', 'Library', Library, '/library', 'collection'),
			navCommand('library-albums', 'Library · Albums', Library, '/library/albums'),
			navCommand('library-artists', 'Library · Artists', Library, '/library/artists'),
			navCommand('library-tracks', 'Library · Tracks', Library, '/library/tracks'),
			navCommand('following', 'Following', Heart, '/following', 'artists concerts releases'),
			navCommand('downloads', 'Downloads', Download, '/downloads', 'queue transfers'),
			navCommand('profile', 'Profile', UserRound, '/profile', 'account connections'),
			navCommand('playlists', 'Playlists', ListMusic, '/playlists')
		];
		if (downloadClientConfigured) {
			cmds.push(navCommand('requests', 'Requests', Inbox, '/requests', 'history wanted'));
		}
		if (integrations.current.localfiles) {
			cmds.push(
				navCommand(
					'listening-room',
					'Listening Room',
					Headphones,
					'/library/local',
					'vinyl local deck'
				)
			);
		}
		if (authStore.isAdmin) {
			cmds.push(navCommand('settings', 'Settings', Settings, '/settings', 'configuration admin'));
			cmds.push(
				navCommand('approvals', 'Approvals', Inbox, '/requests?tab=approvals', 'pending admin')
			);
		}
		cmds.push({
			id: 'theme',
			label: 'Switch theme',
			hint: 'Action',
			icon: Palette,
			keywords: 'appearance dark light contrast',
			run: () => {
				const values = THEME_OPTIONS.map((t) => t.value);
				const next = values[(values.indexOf(appearance.theme) + 1) % values.length];
				appearance.setTheme(next);
			}
		});
		if (playerStore.isPlayerVisible) {
			cmds.push({
				id: 'stage',
				label: 'Open full-screen player',
				hint: 'Action',
				icon: Maximize2,
				keywords: 'now playing queue stage',
				run: () => playerUi.openStage()
			});
		}
		return cmds;
	});

	const filteredCommands = $derived.by(() => {
		const q = query.trim().toLowerCase();
		if (!q) return allCommands;
		return allCommands.filter(
			(c) => c.label.toLowerCase().includes(q) || c.keywords?.toLowerCase().includes(q)
		);
	});

	const hasSearchQuery = $derived(query.trim().length >= 2);

	// flattened, keyboard-navigable item list: recents → suggestions → search-all → commands
	const itemCount = $derived(
		visibleRecents.length + suggestions.length + (hasSearchQuery ? 1 : 0) + filteredCommands.length
	);

	function clampActive() {
		if (activeIndex >= itemCount) activeIndex = Math.max(0, itemCount - 1);
		if (activeIndex < 0) activeIndex = 0;
	}

	$effect(() => {
		void itemCount;
		clampActive();
	});

	function coverUrl(result: SuggestResult): string {
		return result.type === 'artist'
			? getApiUrl(`/api/v1/covers/artist/${result.musicbrainz_id}?size=250`)
			: getApiUrl(`/api/v1/covers/release-group/${result.musicbrainz_id}?size=250`);
	}

	function handleInput() {
		clearTimeout(debounceTimeout);
		abortController?.abort();
		abortController = null;
		activeIndex = 0;

		if (!hasSearchQuery) {
			suggestions = [];
			loading = false;
			return;
		}

		loading = true;
		debounceTimeout = setTimeout(async () => {
			abortController = new AbortController();
			const generation = ++fetchGeneration;
			try {
				const data = await api.global.get<{ results?: SuggestResult[] }>(
					API.search.suggest(query.trim(), 5),
					{ signal: abortController.signal }
				);
				if (generation !== fetchGeneration) return;
				suggestions = data.results ?? [];
				imageErrors = {};
			} catch (e) {
				if (!isAbortError(e)) suggestions = [];
			} finally {
				if (generation === fetchGeneration) loading = false;
			}
		}, 200);
	}

	function close() {
		dialogEl?.close();
	}

	function reset() {
		query = '';
		suggestions = [];
		activeIndex = 0;
		loading = false;
	}

	function selectSuggestion(result: SuggestResult) {
		recordRecent({
			kind: 'suggestion',
			title: result.title,
			type: result.type,
			mbid: result.musicbrainz_id
		});
		close();
		const routeId = result.type === 'artist' ? '/artist/[id]' : '/album/[id]';
		goto(resolve(routeId, { id: result.musicbrainz_id }));
	}

	function startStation(result: SuggestResult) {
		close();
		void launchRadio(
			{
				seed_type: result.type === 'artist' ? 'artist' : 'album',
				seed_id: result.musicbrainz_id
			},
			ytConfigured
		);
	}

	function searchAll() {
		if (!query.trim()) return;
		const q = query.trim();
		recordRecent({ kind: 'query', query: q });
		close();
		goto(`/search?q=${encodeURIComponent(q)}`);
	}

	function runCommand(cmd: Command) {
		close();
		cmd.run();
	}

	function activateIndex(index: number) {
		if (index < visibleRecents.length) {
			activateRecent(visibleRecents[index]);
			return;
		}
		const rest = index - visibleRecents.length;
		if (rest < suggestions.length) {
			selectSuggestion(suggestions[rest]);
			return;
		}
		if (hasSearchQuery && rest === suggestions.length) {
			searchAll();
			return;
		}
		const cmd = filteredCommands[rest - suggestions.length - (hasSearchQuery ? 1 : 0)];
		if (cmd) runCommand(cmd);
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'ArrowDown') {
			e.preventDefault();
			activeIndex = Math.min(activeIndex + 1, itemCount - 1);
		} else if (e.key === 'ArrowUp') {
			e.preventDefault();
			activeIndex = Math.max(activeIndex - 1, 0);
		} else if (e.key === 'Enter') {
			e.preventDefault();
			if (itemCount > 0) {
				activateIndex(activeIndex);
			} else if (query.trim()) {
				searchAll();
			}
		}
	}

	function handleGlobalKeydown(e: KeyboardEvent) {
		const target = e.target as HTMLElement;
		const inField =
			target?.tagName === 'INPUT' ||
			target?.tagName === 'TEXTAREA' ||
			target?.tagName === 'SELECT' ||
			target?.isContentEditable;

		if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
			e.preventDefault();
			if (dialogEl?.open) close();
			else open();
			return;
		}
		if (e.key === '/' && !inField && !dialogEl?.open) {
			e.preventDefault();
			open();
		}
	}

	export function open() {
		loadRecents();
		reset();
		dialogEl?.showModal();
		queueMicrotask(() => inputEl?.focus());
	}
</script>

<svelte:window onkeydown={handleGlobalKeydown} />

<dialog id="search_modal" class="modal items-start pt-[12vh]" bind:this={dialogEl} onclose={reset}>
	<div class="dn-palette modal-box max-w-2xl overflow-hidden p-0">
		<div class="flex items-center gap-4 border-b border-base-content/10 px-5 py-4 sm:px-6">
			<Search class="h-5 w-5 shrink-0 opacity-45" />
			<!-- svelte-ignore a11y_autofocus -->
			<input
				bind:this={inputEl}
				bind:value={query}
				oninput={handleInput}
				onkeydown={handleKeydown}
				type="text"
				class="w-full bg-transparent font-display text-lg font-medium outline-none placeholder:text-base-content/30 sm:text-xl"
				placeholder="Search tracks, albums, or commands"
				autofocus
				aria-label="Search music or type a command"
			/>
			{#if loading}
				<span class="loading loading-spinner loading-xs opacity-50"></span>
			{/if}
			<span class="dn-palette__esc hidden sm:inline-flex">Esc to close</span>
		</div>

		<div class="max-h-[52vh] overflow-y-auto py-2">
			{#if visibleRecents.length > 0}
				<p class="dn-palette__group">
					Recent
					<button class="dn-palette__clear" type="button" onclick={clearRecents}>Clear</button>
				</p>
				{#each visibleRecents as entry, i (recentKeyOf(entry))}
					<button
						class="dn-palette__item"
						class:dn-palette__item--active={activeIndex === i}
						onclick={() => activateRecent(entry)}
						onmouseenter={() => (activeIndex = i)}
					>
						<span class="flex h-8 w-8 items-center justify-center rounded bg-base-content/8">
							{#if entry.kind === 'query'}
								<Search class="h-4 w-4 opacity-50" />
							{:else}
								<History class="h-4 w-4 opacity-50" />
							{/if}
						</span>
						<span class="min-w-0 flex-1 truncate text-left">
							{entry.kind === 'query' ? entry.query : entry.title}
						</span>
						{#if entry.kind === 'suggestion'}
							<span class="badge badge-ghost badge-xs capitalize">{entry.type}</span>
						{:else}
							<span class="text-[0.65rem] uppercase tracking-wider opacity-35">Search</span>
						{/if}
					</button>
				{/each}
			{/if}

			{#if suggestions.length > 0}
				<p class="dn-palette__group">Albums & artists</p>
				{#each suggestions as result, i (result.musicbrainz_id)}
					{@const idx = visibleRecents.length + i}
					<div class="dn-palette__item" class:dn-palette__item--active={activeIndex === idx}>
						<button
							class="flex min-w-0 flex-1 items-center gap-[0.9rem] text-left"
							onclick={() => selectSuggestion(result)}
							onmouseenter={() => (activeIndex = idx)}
						>
							{#if !imageErrors[result.musicbrainz_id]}
								<img
									src={coverUrl(result)}
									alt=""
									class="h-8 w-8 shrink-0 rounded object-cover {result.type === 'artist'
										? 'rounded-full'
										: ''}"
									onerror={() => (imageErrors = { ...imageErrors, [result.musicbrainz_id]: true })}
								/>
							{:else}
								<span
									class="flex h-8 w-8 shrink-0 items-center justify-center rounded bg-base-content/10 {result.type ===
									'artist'
										? 'rounded-full'
										: ''}"
								>
									<Disc3 class="h-4 w-4 opacity-40" />
								</span>
							{/if}
							<span class="min-w-0 flex-1 truncate">
								{result.title}
								{#if result.artist}
									<span class="opacity-50">· {result.artist}</span>
								{/if}
							</span>
						</button>
						<span class="badge badge-ghost badge-xs capitalize">{result.type}</span>
						<button
							class="dn-palette__play"
							type="button"
							aria-label="Start station for {result.title}"
							title="Start station"
							onclick={() => startStation(result)}
							onmouseenter={() => (activeIndex = idx)}
						>
							<Play class="h-3 w-3" fill="currentColor" />
						</button>
					</div>
				{/each}
			{/if}

			{#if hasSearchQuery}
				<button
					class="dn-palette__item"
					class:dn-palette__item--active={activeIndex ===
						visibleRecents.length + suggestions.length}
					onclick={searchAll}
					onmouseenter={() => (activeIndex = visibleRecents.length + suggestions.length)}
				>
					<span class="flex h-8 w-8 items-center justify-center rounded bg-base-content/8">
						<Search class="h-4 w-4 opacity-60" />
					</span>
					<span class="min-w-0 flex-1 truncate text-left"
						>Search everything for “{query.trim()}”</span
					>
					<CornerDownLeft class="h-3.5 w-3.5 opacity-40" />
				</button>
			{/if}

			{#if filteredCommands.length > 0}
				<p class="dn-palette__group">
					{query.trim() ? 'Matching commands' : 'Quick commands'}
				</p>
				{#each filteredCommands as cmd, i (cmd.id)}
					{@const idx = visibleRecents.length + suggestions.length + (hasSearchQuery ? 1 : 0) + i}
					<button
						class="dn-palette__item"
						class:dn-palette__item--active={activeIndex === idx}
						onclick={() => runCommand(cmd)}
						onmouseenter={() => (activeIndex = idx)}
					>
						<span class="flex h-8 w-8 items-center justify-center rounded bg-base-content/8">
							<cmd.icon class="h-4 w-4 opacity-70" />
						</span>
						<span class="min-w-0 flex-1 truncate text-left">{cmd.label}</span>
						{#if cmd.hint}
							<span class="text-[0.65rem] uppercase tracking-wider opacity-35">{cmd.hint}</span>
						{/if}
					</button>
				{/each}
			{/if}

			{#if itemCount === 0 && !loading}
				<p class="px-4 py-6 text-center text-sm opacity-50">
					{hasSearchQuery ? 'No matches — press Enter to search everything.' : 'Nothing here.'}
				</p>
			{/if}
		</div>

		<div
			class="flex items-center gap-3 border-t border-base-content/10 px-4 py-2 text-[0.7rem] opacity-45"
		>
			<span><kbd class="kbd kbd-xs">↑</kbd> <kbd class="kbd kbd-xs">↓</kbd> navigate</span>
			<span><kbd class="kbd kbd-xs">↵</kbd> select</span>
			<span class="ml-auto hidden sm:inline"
				><kbd class="kbd kbd-xs">Ctrl</kbd> <kbd class="kbd kbd-xs">K</kbd> anywhere</span
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close">close</button>
	</form>
</dialog>

<ShortcutsHelp />

<style>
	.dn-palette {
		border: 1px solid var(--dn-hairline);
		box-shadow: var(--dn-shadow-4);
		background: oklch(from var(--color-base-200) l c h / 0.97);
		backdrop-filter: blur(24px) saturate(1.05);
		-webkit-backdrop-filter: blur(24px) saturate(1.05);
	}

	.dn-palette__esc {
		align-items: center;
		padding: 0.28rem 0.65rem;
		border-radius: 0.5rem;
		border: 1px solid var(--dn-hairline);
		background: oklch(from var(--color-base-content) l c h / 0.05);
		font-family: var(--font-mono);
		font-size: 0.58rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		white-space: nowrap;
		color: oklch(from var(--color-base-content) l c h / 0.45);
	}

	.dn-palette__group {
		display: flex;
		align-items: center;
		gap: 0.9rem;
		padding: 0.85rem 1.4rem 0.4rem;
		font-family: var(--font-mono);
		font-size: 0.6rem;
		font-weight: 700;
		letter-spacing: 0.22em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.38);
	}
	.dn-palette__group::after {
		content: '';
		flex: 1;
		height: 1px;
		background: var(--dn-hairline);
	}

	.dn-palette__item {
		display: flex;
		align-items: center;
		gap: 0.9rem;
		width: 100%;
		padding: 0.6rem 1.4rem;
		font-size: 0.95rem;
		color: oklch(from var(--color-base-content) l c h / 0.85);
		transition: background var(--dn-dur-instant) ease;
	}

	.dn-palette__item--active {
		background: oklch(from var(--color-primary) l c h / 0.14);
		color: oklch(from var(--color-base-content) l c h);
	}

	/* "Clear" sits after the group's hairline, at the far right */
	.dn-palette__clear {
		order: 2;
		font-family: var(--font-mono);
		font-size: 0.6rem;
		font-weight: 700;
		letter-spacing: 0.22em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.38);
		transition: color var(--dn-dur-instant) ease;
	}
	.dn-palette__clear:hover,
	.dn-palette__clear:focus-visible {
		color: oklch(from var(--color-base-content) l c h / 0.75);
	}

	/* trailing station-play control: hidden until the row is hovered/active,
	   but always focusable so keyboard users can tab to it */
	.dn-palette__play {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.75rem;
		height: 1.75rem;
		flex-shrink: 0;
		border-radius: 9999px;
		border: 1px solid var(--dn-hairline);
		background: oklch(from var(--color-base-content) l c h / 0.06);
		color: oklch(from var(--color-base-content) l c h / 0.75);
		opacity: 0;
		transition:
			opacity var(--dn-dur-instant) ease,
			background var(--dn-dur-instant) ease,
			color var(--dn-dur-instant) ease;
	}
	.dn-palette__item:hover .dn-palette__play,
	.dn-palette__item--active .dn-palette__play,
	.dn-palette__play:focus-visible {
		opacity: 1;
	}
	.dn-palette__play:hover,
	.dn-palette__play:focus-visible {
		background: oklch(from var(--color-primary) l c h / 0.2);
		color: oklch(from var(--color-base-content) l c h);
	}
</style>
