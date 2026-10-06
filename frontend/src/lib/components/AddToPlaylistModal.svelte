<script lang="ts">
	import { browser } from '$app/environment';
	import { Disc3, Search, Plus, Check, CircleCheck, X } from 'lucide-svelte';
	import { fly } from 'svelte/transition';
	import {
		queueItemToTrackData,
		isRedactedPlaylist,
		type PlaylistSummary
	} from '$lib/api/playlists';
	import { api } from '$lib/api/client';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { PlaylistQueryKeyFactory } from '$lib/queries/playlists/PlaylistQueryKeyFactory';
	import { PlaylistV3Api } from '$lib/queries/playlists/PlaylistV3Api';
	import {
		addPlaylistTracksV3,
		checkPlaylistTracksV3,
		createPlaylistV3
	} from '$lib/queries/playlists/PlaylistV3Mutations.svelte';
	import {
		toPageDetail,
		toPageList,
		trackDataToV3Input,
		transposeMembership
	} from '$lib/queries/playlists/playlistV3Adapter';
	import PlaylistMosaic from './PlaylistMosaic.svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import type { QueueItem } from '$lib/player/types';

	const reducedMotion = browser && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

	// Creating a playlist / adding tracks here changes the user's playlist list and
	// track counts; invalidate the user-scoped list query so /playlists reflects it.
	function invalidatePlaylistList() {
		void invalidateQueriesWithPersister({
			queryKey: PlaylistQueryKeyFactory.list(authStore.user?.id)
		});
	}

	const createMutation = createPlaylistV3();
	const addTracksMutation = addPlaylistTracksV3();
	const checkTracksMutation = checkPlaylistTracksV3();

	let dialogEl: HTMLDialogElement | undefined = $state();
	let pendingTracks: QueueItem[] = [];
	let trackCount = $state(0);
	let playlists = $state<PlaylistSummary[]>([]);
	let loading = $state(true);
	let fetchError = $state<string | null>(null);
	let addedSet = new SvelteSet<string>();
	let addingSet = new SvelteSet<string>();
	let membership = $state<Record<string, number[]>>({});
	let newName = $state('');
	let creating = $state(false);
	let search = $state('');
	let statusMessage = $state<{ text: string; type: 'success' | 'error' } | null>(null);

	let filteredPlaylists = $derived.by(() => {
		const q = search.trim().toLowerCase();
		return q ? playlists.filter((p) => p.name.toLowerCase().includes(q)) : playlists;
	});

	function existingCount(playlistId: string): number {
		return membership[playlistId]?.length ?? 0;
	}

	function allTracksExist(playlistId: string): boolean {
		return trackCount > 0 && existingCount(playlistId) >= trackCount;
	}

	function someTracksExist(playlistId: string): boolean {
		const count = existingCount(playlistId);
		return count > 0 && count < trackCount;
	}

	export function open(items: QueueItem[]) {
		pendingTracks = items;
		trackCount = items.length;
		addedSet.clear();
		addingSet.clear();
		membership = {};
		newName = '';
		search = '';
		fetchError = null;
		statusMessage = null;
		loading = true;
		dialogEl?.showModal();
		loadPlaylists();
	}

	export function close() {
		dialogEl?.close();
	}

	async function loadPlaylists() {
		try {
			// Only the user's own playlists are valid add targets (D4).
			const answer = await api.global.v3.GET(PlaylistV3Api.list());
			playlists = toPageList(answer.playlists).filter(
				(p): p is PlaylistSummary => !isRedactedPlaylist(p) && p.is_owner
			);
			if (pendingTracks.length > 0) {
				const trackIdentifiers = pendingTracks.map((t) => ({
					track_name: t.trackName,
					artist_name: t.artistName,
					album_name: t.albumName
				}));
				const checked = await checkTracksMutation.mutateAsync({ tracks: trackIdentifiers });
				membership = transposeMembership(checked.membership);
			}
		} catch {
			fetchError = "Couldn't load your playlists.";
		} finally {
			loading = false;
		}
	}

	function showStatus(text: string, type: 'success' | 'error') {
		statusMessage = { text, type };
		setTimeout(() => {
			statusMessage = null;
		}, 3000);
	}

	async function handleAdd(playlist: PlaylistSummary) {
		if (addedSet.has(playlist.id) || addingSet.has(playlist.id)) return;
		if (allTracksExist(playlist.id)) return;
		if (pendingTracks.length === 0) return;
		addingSet.add(playlist.id);
		try {
			const existingIndices = new Set(membership[playlist.id] ?? []);
			const tracksToAdd = pendingTracks.filter((_, i) => !existingIndices.has(i));
			if (tracksToAdd.length === 0) {
				addedSet.add(playlist.id);
				return;
			}
			const trackData = tracksToAdd.map(queueItemToTrackData);
			await addTracksMutation.mutateAsync({
				id: playlist.id,
				tracks: trackData.map(trackDataToV3Input)
			});
			addedSet.add(playlist.id);
			const allIndices = Array.from({ length: trackCount }, (_, i) => i);
			membership = { ...membership, [playlist.id]: allIndices };
			playlists = playlists.map((p) =>
				p.id === playlist.id ? { ...p, track_count: p.track_count + trackData.length } : p
			);
			invalidatePlaylistList();
			const addedCount = trackData.length;
			if (existingIndices.size > 0) {
				showStatus(
					`Filed ${addedCount} track${addedCount === 1 ? '' : 's'} in "${playlist.name}". ${existingIndices.size} ${existingIndices.size === 1 ? 'track was' : 'tracks were'} already in it.`,
					'success'
				);
			} else {
				showStatus(`Filed the tracks in "${playlist.name}".`, 'success');
			}
		} catch {
			showStatus("Couldn't add those tracks.", 'error');
		} finally {
			addingSet.delete(playlist.id);
		}
	}

	async function handleCreate() {
		const name = newName.trim();
		if (!name || creating || pendingTracks.length === 0) return;
		creating = true;
		try {
			const created = await createMutation.mutateAsync(name);
			const detail = toPageDetail(created);
			const trackData = pendingTracks.map(queueItemToTrackData);
			await addTracksMutation.mutateAsync({
				id: detail.id,
				tracks: trackData.map(trackDataToV3Input)
			});
			addedSet.add(detail.id);
			const allIndices = Array.from({ length: trackCount }, (_, i) => i);
			membership = { ...membership, [detail.id]: allIndices };
			playlists = [{ ...detail, track_count: trackData.length }, ...playlists];
			invalidatePlaylistList();
			newName = '';
			showStatus(`Pressed "${name}" and filed the tracks.`, 'success');
		} catch {
			showStatus("Couldn't create that playlist.", 'error');
		} finally {
			creating = false;
		}
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			e.preventDefault();
			handleCreate();
		}
	}

	function sleeveState(id: string): 'added' | 'adding' | 'partial' | 'idle' {
		if (addedSet.has(id) || allTracksExist(id)) return 'added';
		if (addingSet.has(id)) return 'adding';
		if (someTracksExist(id)) return 'partial';
		return 'idle';
	}

	function sleeveAria(p: PlaylistSummary): string {
		const s = sleeveState(p.id);
		if (s === 'added') return 'Already added';
		if (s === 'adding') return 'Adding tracks';
		if (s === 'partial') return `Add the remaining tracks to ${p.name}`;
		return `Add to ${p.name}`;
	}

	let trackLabel = $derived(trackCount === 1 ? '1 track' : `${trackCount} tracks`);
	let canAdd = $derived(trackCount > 0);
</script>

<dialog bind:this={dialogEl} class="modal crate-modal">
	<div class="modal-box crate-box max-w-2xl overflow-hidden p-0">
		<div class="crate-halo" aria-hidden="true"></div>

		<div class="relative flex flex-col gap-4 p-5 sm:p-6">
			<header class="flex items-center gap-3">
				<div class="crate-disc" aria-hidden="true">
					<Disc3 class="h-6 w-6 {reducedMotion ? '' : 'vinyl-spin'}" />
				</div>
				<div class="min-w-0 flex-1">
					<h3 class="text-lg font-extrabold leading-tight tracking-tight">Add to the crate</h3>
					<p class="text-xs text-base-content/55">{trackLabel} ready to file</p>
				</div>
				<form method="dialog">
					<button class="btn btn-circle btn-ghost btn-sm" aria-label="Close">
						<X class="h-4 w-4" />
					</button>
				</form>
			</header>

			<!-- Press a new record -->
			<div class="press-row flex items-center gap-2 rounded-2xl px-3 py-2">
				<Disc3 class="h-4 w-4 shrink-0 text-base-content/40" />
				<input
					type="text"
					class="grow border-0 bg-transparent text-sm outline-none placeholder:text-base-content/40 focus:ring-0"
					placeholder="New playlist name"
					bind:value={newName}
					onkeydown={handleKeydown}
					disabled={creating}
				/>
				<button
					class="press-btn btn btn-circle btn-accent btn-sm shrink-0"
					onclick={handleCreate}
					disabled={!newName.trim() || creating || !canAdd}
					aria-label="Create playlist"
					title="Press a new record"
				>
					{#if creating}
						<span class="loading loading-spinner loading-xs"></span>
					{:else}
						<Plus class="h-4 w-4" />
					{/if}
				</button>
			</div>

			{#if playlists.length > 0}
				<label class="search-bar flex items-center gap-2 rounded-xl px-3 py-1.5">
					<Search class="h-4 w-4 shrink-0 text-base-content/40" />
					<input
						type="text"
						class="grow border-0 bg-transparent text-sm outline-none placeholder:text-base-content/40 focus:ring-0"
						placeholder="Search your crate…"
						bind:value={search}
					/>
				</label>
			{/if}

			<div class="crate-scroll scrollbar-hide -mx-1 max-h-[min(56vh,27rem)] overflow-y-auto px-1">
				{#if loading}
					<div class="crate-grid">
						{#each Array(8) as _, i (i)}
							<div class="sleeve-skel" data-testid="playlist-skeleton"></div>
						{/each}
					</div>
				{:else if fetchError}
					<div role="alert" class="alert alert-error">
						<span>{fetchError}</span>
					</div>
				{:else if playlists.length === 0}
					<div class="flex flex-col items-center gap-2 py-10 text-center">
						<div class="crate-disc crate-disc--lg" aria-hidden="true">
							<Disc3 class="h-7 w-7 text-base-content/45" />
						</div>
						<p class="text-sm font-semibold text-base-content/70">
							You haven't created any playlists yet.
						</p>
						<p class="max-w-[16rem] text-xs text-base-content/45">
							Name one above and press your first record.
						</p>
					</div>
				{:else}
					<div class="crate-grid">
						{#each filteredPlaylists as playlist, i (playlist.id)}
							{@const state = sleeveState(playlist.id)}
							<button
								class="sleeve {state === 'added'
									? 'is-added'
									: state === 'adding'
										? 'is-adding'
										: state === 'partial'
											? 'is-partial'
											: ''}"
								onclick={() => handleAdd(playlist)}
								disabled={state === 'added' || state === 'adding' || !canAdd}
								aria-label={sleeveAria(playlist)}
								title={state === 'partial'
									? `${existingCount(playlist.id)} of ${trackCount} already in "${playlist.name}"`
									: sleeveAria(playlist)}
								in:fly={{
									y: reducedMotion ? 0 : 14,
									duration: reducedMotion ? 0 : 260,
									delay: reducedMotion ? 0 : Math.min(i, 11) * 35
								}}
							>
								<div class="sleeve-art">
									<div class="vinyl-wrap" aria-hidden="true">
										<div class="vinyl-disc"></div>
									</div>
									<div class="sleeve-cover">
										<PlaylistMosaic
											coverUrls={playlist.cover_urls}
											customCoverUrl={playlist.custom_cover_url}
											size="w-full h-full"
											rounded="rounded-none"
										/>
									</div>
									{#if state === 'adding'}
										<div class="sleeve-sweep pointer-events-none absolute inset-0 z-20"></div>
									{/if}
									<div class="sleeve-badge sleeve-badge--{state}">
										{#if state === 'added'}
											<Check class="h-3.5 w-3.5" />
										{:else if state === 'adding'}
											<span class="loading loading-spinner loading-xs"></span>
										{:else if state === 'partial'}
											<CircleCheck class="h-3.5 w-3.5" />
										{:else}
											<Plus class="h-3.5 w-3.5" />
										{/if}
									</div>
								</div>
								<div class="sleeve-meta">
									<p class="truncate text-sm font-semibold text-base-content">{playlist.name}</p>
									<p class="truncate text-[11px] text-base-content/45">
										{playlist.track_count} track{playlist.track_count === 1 ? '' : 's'}{state ===
										'partial'
											? ` · ${existingCount(playlist.id)} here`
											: ''}
									</p>
								</div>
							</button>
						{/each}
					</div>
					{#if filteredPlaylists.length === 0}
						<p class="py-8 text-center text-sm text-base-content/45">
							No playlists match "{search}".
						</p>
					{/if}
				{/if}
			</div>

			{#if statusMessage}
				<div
					role="alert"
					class="status-strip {statusMessage.type === 'success'
						? 'status-strip--ok'
						: 'status-strip--err'}"
					transition:fly={{ y: 8, duration: reducedMotion ? 0 : 200 }}
				>
					<span class="text-sm">{statusMessage.text}</span>
				</div>
			{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>Close</button>
	</form>
</dialog>

<style>
	/* Blurred, dimmed backdrop so the crate floats. */
	.crate-modal::backdrop {
		background: oklch(from var(--color-base-100) l c h / 0.55);
		backdrop-filter: blur(8px) saturate(0.9);
		-webkit-backdrop-filter: blur(8px) saturate(0.9);
	}

	.crate-box {
		position: relative;
		border: 1px solid oklch(from var(--color-base-content) l c h / 0.08);
		border-radius: 1.5rem;
		background: linear-gradient(
			180deg,
			oklch(from var(--color-base-300) l c h / 0.92),
			oklch(from var(--color-base-100) l c h / 0.97)
		);
		box-shadow:
			0 32px 70px -28px oklch(from var(--color-base-100) l c h / 0.95),
			inset 0 1px 0 oklch(from var(--color-base-content) l c h / 0.06);
	}

	/* Soft accent halo bleeding from the top, matching the app's hero glow. */
	.crate-halo {
		position: absolute;
		inset: -30% -20% auto -20%;
		height: 60%;
		background: radial-gradient(
			60% 80% at 50% 0%,
			oklch(from var(--color-primary) l c h / 0.16),
			transparent 70%
		);
		pointer-events: none;
		z-index: 0;
	}

	/* Spinning label disc in the header / empty state. */
	.crate-disc {
		display: grid;
		place-items: center;
		height: 2.75rem;
		width: 2.75rem;
		flex-shrink: 0;
		border-radius: 9999px;
		color: var(--color-primary);
		background:
			radial-gradient(
				circle at 35% 30%,
				oklch(from var(--color-base-content) l c h / 0.14),
				transparent 55%
			),
			radial-gradient(circle at center, var(--color-base-300), var(--color-base-100) 92%);
		box-shadow:
			inset 0 0 0 1px oklch(from var(--color-base-content) l c h / 0.1),
			0 0 22px oklch(from var(--color-primary) l c h / 0.18);
	}
	.crate-disc--lg {
		height: 3.5rem;
		width: 3.5rem;
	}

	/* "Press a new record" + search fields styled as inset slots. */
	.press-row,
	.search-bar {
		border: 1px solid oklch(from var(--color-base-content) l c h / 0.08);
		background: oklch(from var(--color-base-100) l c h / 0.6);
		transition:
			border-color 0.2s ease,
			box-shadow 0.2s ease;
	}
	.press-row:focus-within,
	.search-bar:focus-within {
		border-color: oklch(from var(--color-primary) l c h / 0.5);
		box-shadow: 0 0 0 3px oklch(from var(--color-primary) l c h / 0.12);
	}
	.press-btn {
		box-shadow: 0 0 18px oklch(from var(--color-primary) l c h / 0.35);
	}

	.crate-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.85rem;
		padding: 0.25rem 0 0.5rem;
	}
	@media (min-width: 480px) {
		.crate-grid {
			grid-template-columns: repeat(3, minmax(0, 1fr));
		}
	}
	@media (min-width: 700px) {
		.crate-grid {
			grid-template-columns: repeat(4, minmax(0, 1fr));
		}
	}

	.sleeve-skel {
		aspect-ratio: 1;
		border-radius: 0.6rem;
		background: oklch(from var(--color-base-300) l c h / 0.6);
		animation: sleeve-pulse 1.4s ease-in-out infinite;
	}

	/* Local light-sweep for the "filing" moment (the global crate-sweep relies on
	   removed v4 tokens). The crate-sweep keyframes themselves are token-free. */
	.sleeve-sweep {
		border-radius: 0.6rem;
		background: linear-gradient(
			115deg,
			transparent 28%,
			oklch(from var(--color-primary) l c h / 0.3) 50%,
			transparent 72%
		);
		animation: crate-sweep 0.75s ease-out;
	}
	@keyframes sleeve-pulse {
		50% {
			opacity: 0.45;
		}
	}

	/* A playlist sleeve: the cover is the sleeve, a vinyl peeks out the right edge. */
	.sleeve {
		position: relative;
		z-index: 1;
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		padding: 0;
		text-align: left;
		background: none;
		border: none;
		cursor: pointer;
		perspective: 720px;
	}
	.sleeve:disabled {
		cursor: default;
	}
	/* Hovered/added sleeves rise above neighbours so the peeking record isn't covered. */
	.sleeve:hover,
	.sleeve:focus-visible,
	.sleeve.is-added,
	.sleeve.is-adding {
		z-index: 6;
	}

	.sleeve-art {
		position: relative;
		aspect-ratio: 1;
		border-radius: 0.6rem;
		transform-style: preserve-3d;
		transition:
			transform 0.28s var(--ease-spring, cubic-bezier(0.23, 1, 0.32, 1)),
			box-shadow 0.28s var(--ease-spring, cubic-bezier(0.23, 1, 0.32, 1));
		box-shadow: 0 8px 18px -12px oklch(from var(--color-base-100) l c h / 0.95);
	}

	/* The sleeve cover sits on top of the vinyl. Absolute-fill so its height tracks
	   the aspect-ratio box (a % height would collapse to the cover's content size). */
	.sleeve-cover {
		position: absolute;
		inset: 0;
		z-index: 2;
		overflow: hidden;
		border-radius: 0.6rem;
		box-shadow:
			inset 0 0 0 1px oklch(from var(--color-base-content) l c h / 0.1),
			inset -10px 0 18px -14px oklch(from var(--color-base-100) l c h / 0.9);
	}
	/* a faint sleeve "opening" highlight down the right spine */
	.sleeve-cover::after {
		content: '';
		position: absolute;
		inset: 6% 0 6% auto;
		right: 0;
		width: 2px;
		background: linear-gradient(
			oklch(from var(--color-base-content) l c h / 0),
			oklch(from var(--color-base-content) l c h / 0.18),
			oklch(from var(--color-base-content) l c h / 0)
		);
		z-index: 3;
	}

	.vinyl-wrap {
		position: absolute;
		inset: 8% -2% 8% auto;
		right: 0;
		aspect-ratio: 1;
		z-index: 1;
		display: grid;
		place-items: center;
		transform: translateX(20%);
		transition: transform 0.32s var(--ease-overshoot, cubic-bezier(0.34, 1.56, 0.64, 1));
	}
	.vinyl-disc {
		position: relative;
		height: 100%;
		aspect-ratio: 1;
		border-radius: 9999px;
		background:
			radial-gradient(circle at center, var(--color-primary) 0 17%, transparent 18%),
			repeating-radial-gradient(
				circle at center,
				transparent 0 2px,
				oklch(from var(--color-base-content) l c h / 0.16) 2px 3px
			),
			radial-gradient(
				circle at 36% 30%,
				oklch(from var(--color-base-content) l c h / 0.28),
				transparent 48%
			),
			radial-gradient(circle at center, var(--color-base-300) 0%, var(--color-base-100) 95%);
		box-shadow:
			inset 0 0 0 1px oklch(from var(--color-base-content) l c h / 0.14),
			0 10px 20px -10px oklch(from var(--color-base-100) l c h / 0.95);
	}
	.vinyl-disc::after {
		content: '';
		position: absolute;
		left: 50%;
		top: 50%;
		height: 5%;
		width: 5%;
		transform: translate(-50%, -50%);
		border-radius: 9999px;
		background: var(--color-base-100);
	}

	/* Hover: lift, tilt toward the viewer, slide the record further out + spin. */
	.sleeve:not(:disabled):hover .sleeve-art,
	.sleeve:not(:disabled):focus-visible .sleeve-art {
		transform: translateY(-4px) rotateX(5deg) rotateY(-9deg);
		box-shadow:
			0 22px 40px -20px oklch(from var(--color-base-100) l c h / 0.95),
			0 0 24px oklch(from var(--color-primary) l c h / 0.22);
	}
	.sleeve:not(:disabled):hover .vinyl-wrap,
	.sleeve:not(:disabled):focus-visible .vinyl-wrap {
		transform: translateX(38%);
	}
	.sleeve:not(:disabled):hover .vinyl-disc,
	.sleeve:not(:disabled):focus-visible .vinyl-disc {
		animation: spin-vinyl 4.2s linear infinite;
	}

	/* Adding: the record slots into the sleeve, with a light sweep. */
	.sleeve.is-adding .vinyl-wrap {
		transform: translateX(-18%);
	}
	.sleeve.is-adding .sleeve-art {
		box-shadow:
			0 18px 36px -18px oklch(from var(--color-base-100) l c h / 0.95),
			0 0 26px oklch(from var(--color-primary) l c h / 0.3);
	}

	/* Added: tucked in, ringed and glowing. */
	.sleeve.is-added .vinyl-wrap {
		transform: translateX(-18%);
		opacity: 0.85;
	}
	.sleeve.is-added .sleeve-art {
		box-shadow:
			0 10px 22px -14px oklch(from var(--color-base-100) l c h / 0.95),
			0 0 0 2px oklch(from var(--color-primary) l c h / 0.6),
			0 0 26px oklch(from var(--color-primary) l c h / 0.28);
	}
	.sleeve.is-added .sleeve-cover {
		filter: saturate(0.85);
	}

	.sleeve-badge {
		position: absolute;
		bottom: 0.4rem;
		right: 0.4rem;
		z-index: 4;
		display: grid;
		place-items: center;
		height: 1.5rem;
		width: 1.5rem;
		border-radius: 9999px;
		color: var(--color-base-100);
		background: oklch(from var(--color-base-content) l c h / 0.55);
		backdrop-filter: blur(4px);
		box-shadow: 0 2px 8px -2px oklch(from var(--color-base-100) l c h / 0.9);
		opacity: 0;
		transform: scale(0.8);
		transition:
			opacity 0.18s ease,
			transform 0.2s var(--ease-overshoot, cubic-bezier(0.34, 1.56, 0.64, 1)),
			background-color 0.18s ease;
	}
	.sleeve:hover .sleeve-badge,
	.sleeve:focus-visible .sleeve-badge,
	.sleeve-badge--added,
	.sleeve-badge--adding,
	.sleeve-badge--partial {
		opacity: 1;
		transform: scale(1);
	}
	.sleeve-badge--added {
		background: var(--color-primary);
		color: var(--color-primary-content);
	}
	.sleeve-badge--partial {
		background: var(--color-warning);
		color: var(--color-warning-content);
	}

	.status-strip {
		border-radius: 0.85rem;
		padding: 0.55rem 0.9rem;
		border: 1px solid transparent;
	}
	.status-strip--ok {
		background: oklch(from var(--color-primary) l c h / 0.14);
		border-color: oklch(from var(--color-primary) l c h / 0.4);
		color: oklch(from var(--color-primary) l c h);
	}
	.status-strip--err {
		background: oklch(from var(--color-error) l c h / 0.14);
		border-color: oklch(from var(--color-error) l c h / 0.4);
		color: var(--color-error);
	}

	@media (prefers-reduced-motion: reduce) {
		.sleeve-art,
		.vinyl-wrap,
		.sleeve-badge,
		.sleeve-skel {
			transition: none !important;
			animation: none !important;
		}
		.sleeve:hover .sleeve-art,
		.sleeve:focus-visible .sleeve-art {
			transform: none;
		}
		.sleeve:hover .vinyl-wrap {
			transform: translateX(20%);
		}
	}
</style>
