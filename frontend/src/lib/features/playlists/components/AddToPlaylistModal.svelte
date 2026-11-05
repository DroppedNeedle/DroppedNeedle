<script lang="ts">
	import { browser } from '$app/environment';
	import { Disc3, Search, Plus, Check, CircleCheck } from 'lucide-svelte';
	import { fly } from '$lib/ui/motion';
	import {
		queueItemToTrackData,
		isRedactedPlaylist,
		type PlaylistSummary
	} from '$lib/api/playlists';
	import { fetchPlaylistListV3 } from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
	import {
		createAddPlaylistTracksV3Mutation,
		createCheckPlaylistTracksV3Mutation,
		createPlaylistV3Mutation
	} from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import {
		toPageDetail,
		toPageList,
		trackDataToV3Input,
		transposeMembership
	} from '$lib/features/playlists/queries/playlistV3Adapter';
	import PlaylistMosaic from '$lib/media/PlaylistMosaic.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import type { QueueItem } from '$lib/features/player/playback/types';

	const CHECK_CHUNK = 1000;
	const reducedMotion = browser && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

	const createMutation = createPlaylistV3Mutation();
	const addTracksMutation = createAddPlaylistTracksV3Mutation();
	const checkTracksMutation = createCheckPlaylistTracksV3Mutation();

	let shown = $state(false);
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
		shown = true;
		loadPlaylists();
	}

	export function close() {
		shown = false;
	}

	async function loadPlaylists() {
		try {
			// Only the user's own playlists are valid add targets.
			playlists = toPageList(await fetchPlaylistListV3()).filter(
				(p): p is PlaylistSummary => !isRedactedPlaylist(p) && p.is_owner
			);
			if (pendingTracks.length > 0) {
				const trackIdentifiers = pendingTracks.map((t) => ({
					track_name: t.trackName,
					artist_name: t.artistName,
					album_name: t.albumName
				}));
				// The server checks at most CHECK_CHUNK tracks per call; each
				// answer's keys index into its own chunk.
				const merged: Record<string, string[]> = {};
				for (let start = 0; start < trackIdentifiers.length; start += CHECK_CHUNK) {
					const checked = await checkTracksMutation.mutateAsync({
						tracks: trackIdentifiers.slice(start, start + CHECK_CHUNK)
					});
					for (const [index, playlistIds] of Object.entries(checked.membership)) {
						merged[String(start + Number(index))] = playlistIds;
					}
				}
				membership = transposeMembership(merged);
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

<Modal
	bind:open={shown}
	title="Add to the crate"
	description="{trackLabel} ready to file"
	icon={Disc3}
	size="lg"
	closeButton
>
	<div class="flex flex-col gap-4">
		<div class="flex items-center gap-2">
			<Input
				icon={Disc3}
				label="New playlist name"
				placeholder="New playlist name"
				class="grow"
				bind:value={newName}
				onkeydown={handleKeydown}
				disabled={creating}
			/>
			<IconButton
				variant="primary"
				shape="square"
				icon={Plus}
				label="Create playlist"
				class="shrink-0"
				loading={creating}
				onclick={handleCreate}
				disabled={!newName.trim() || !canAdd}
			/>
		</div>

		{#if playlists.length > 0}
			<Input
				size="sm"
				icon={Search}
				label="Search your crate"
				placeholder="Search your crate…"
				class="w-full"
				bind:value={search}
			/>
		{/if}

		<div class="crate-scroll scrollbar-hide max-h-[min(56vh,27rem)] overflow-y-auto">
			{#if loading}
				<ul class="crate-list">
					{#each Array(6) as _, i (i)}
						<li class="crate-row" data-testid="playlist-skeleton">
							<div class="skeleton size-12 shrink-0 rounded-md"></div>
							<div class="flex flex-1 flex-col gap-2">
								<div class="skeleton h-4 w-1/2"></div>
								<div class="skeleton h-3 w-1/4"></div>
							</div>
						</li>
					{/each}
				</ul>
			{:else if fetchError}
				<div role="alert" class="alert alert-error">
					<span>{fetchError}</span>
				</div>
			{:else if playlists.length === 0}
				<div class="flex flex-col items-center gap-2 py-10 text-center">
					<Disc3 class="mb-1 size-10 text-muted" aria-hidden="true" />
					<p class="text-sm font-semibold text-muted">You haven't created any playlists yet.</p>
					<p class="max-w-[16rem] text-xs text-muted">
						Name one above and press your first record.
					</p>
				</div>
			{:else}
				<ul class="crate-list">
					{#each filteredPlaylists as playlist, i (playlist.id)}
						{@const state = sleeveState(playlist.id)}
						<li
							in:fly={{
								y: reducedMotion ? 0 : 10,
								duration: reducedMotion ? 0 : 220,
								delay: reducedMotion ? 0 : Math.min(i, 11) * 30
							}}
						>
							<button
								type="button"
								class="crate-row crate-row--button is-{state}"
								onclick={() => handleAdd(playlist)}
								disabled={state === 'added' || state === 'adding' || !canAdd}
								aria-label={sleeveAria(playlist)}
								title={state === 'partial'
									? `${existingCount(playlist.id)} of ${trackCount} already in "${playlist.name}"`
									: sleeveAria(playlist)}
							>
								<span class="crate-art">
									<PlaylistMosaic
										coverUrls={playlist.cover_urls}
										customCoverUrl={playlist.custom_cover_url}
										size="w-full h-full"
										rounded="rounded-none"
									/>
								</span>
								<span class="min-w-0 flex-1">
									<span class="crate-name">{playlist.name}</span>
									<span class="crate-meta">
										{playlist.track_count} track{playlist.track_count === 1 ? '' : 's'}{state ===
										'partial'
											? ` · ${existingCount(playlist.id)} here`
											: ''}
									</span>
								</span>
								<span class="crate-badge" aria-hidden="true">
									{#if state === 'added'}
										<Check class="size-4" />
									{:else if state === 'adding'}
										<span class="loading loading-spinner loading-xs"></span>
									{:else if state === 'partial'}
										<CircleCheck class="size-4" />
									{:else}
										<Plus class="size-4" />
									{/if}
								</span>
							</button>
						</li>
					{/each}
				</ul>
				{#if filteredPlaylists.length === 0}
					<p class="py-8 text-center text-sm text-muted">
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
</Modal>

<style>
	.crate-list {
		border-block: 1px solid var(--dn-line);
	}
	.crate-list > :global(* + *) {
		border-top: 1px solid var(--dn-line);
	}

	.crate-row {
		display: flex;
		width: 100%;
		align-items: center;
		gap: 0.875rem;
		padding: 0.625rem 0.5rem;
		text-align: left;
	}
	.crate-row--button {
		border-radius: 0.375rem;
		transition: background-color 150ms;
	}
	.crate-row--button:not(:disabled):hover {
		background-color: var(--color-base-300);
	}
	.crate-row--button:focus-visible {
		outline-offset: -2px;
	}
	.crate-row--button:disabled {
		cursor: default;
	}

	.crate-art {
		flex: none;
		width: 3rem;
		height: 3rem;
		overflow: hidden;
		border-radius: 0.3125rem;
	}

	.crate-name {
		display: block;
		overflow: hidden;
		font-size: 0.9375rem;
		font-weight: 650;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.crate-meta {
		display: block;
		margin-top: 0.125rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	/* the row's state at the right: add, adding, partly there, added */
	.crate-badge {
		display: grid;
		flex: none;
		place-items: center;
		width: 2rem;
		height: 2rem;
		border: 1px solid var(--dn-edge);
		border-radius: 0.5rem;
		color: var(--color-base-content);
		transition:
			background-color 150ms,
			border-color 150ms;
	}
	.crate-row--button:not(:disabled):hover .crate-badge {
		border-color: var(--dn-action-edge);
		background-color: var(--dn-action);
		color: var(--dn-action-content);
	}
	.is-added .crate-badge {
		border-color: var(--dn-action-edge);
		background-color: var(--dn-action);
		color: var(--dn-action-content);
	}
	.is-partial .crate-badge {
		border-color: color-mix(in oklab, var(--color-warning) 60%, transparent);
		color: var(--color-warning);
	}

	.status-strip {
		border-radius: var(--radius-box);
		padding: 0.55rem 0.9rem;
		border: 1px solid transparent;
	}
	.status-strip--ok {
		background: color-mix(in oklab, var(--color-success) 12%, transparent);
		border-color: color-mix(in oklab, var(--color-success) 45%, transparent);
	}
	.status-strip--err {
		background: color-mix(in oklab, var(--color-error) 12%, transparent);
		border-color: color-mix(in oklab, var(--color-error) 45%, transparent);
		color: var(--color-error);
	}
</style>
