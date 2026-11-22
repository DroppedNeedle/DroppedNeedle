<script lang="ts">
	import { goto } from '$app/navigation';
	import { onDestroy, untrack } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import type { PlaylistDetail } from '$lib/api/playlists';
	import { playlistTrackToQueueItem } from '$lib/features/player/playback/queueHelpers';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import { toastStore } from '$lib/stores/toast';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		getPlaylistDetailV3Query,
		getPlaylistSourcesV3QueryOptions,
		type PlaylistDetailV3
	} from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
	import {
		createDeletePlaylistV3Mutation,
		createLeavePlaylistV3Mutation,
		createAcquirePlaylistV3Mutation,
		forgetPlaylistSources,
		getAcquireProgressV3,
		refreshPlaylist
	} from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import {
		privatePlaylistLine,
		privatePlaylistStub,
		toPageDetail
	} from '$lib/features/playlists/queries/playlistV3Adapter';
	import { queryClient } from '$lib/queries/QueryClient';
	import { withBasePath } from '$lib/utils/basePath';
	import { Music, Download, Lock, LogOut } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import type { PageData } from './$types';
	import PlaylistHeader from '$lib/features/playlists/components/PlaylistHeader.svelte';
	import PlaylistTrackList from '$lib/features/playlists/components/PlaylistTrackList.svelte';
	import DeletePlaylistModal from '$lib/features/playlists/components/DeletePlaylistModal.svelte';
	import SharePlaylistModal from '$lib/features/playlists/components/SharePlaylistModal.svelte';
	import PlaylistNotice from '$lib/features/playlists/components/PlaylistNotice.svelte';

	let { data }: { data: PageData } = $props();

	const detailQuery = getPlaylistDetailV3Query(
		() => data.playlistId,
		() => true
	);
	const leaveMutation = createLeavePlaylistV3Mutation();
	const deleteMutation = createDeletePlaylistV3Mutation();
	const acquireMissing = createAcquirePlaylistV3Mutation();

	// A local mutable copy of the (full) playlist so child components can keep
	// applying optimistic updates; redaction/loading/error leave it null.
	let playlist = $state<PlaylistDetail | null>(null);
	let deleting = $state(false);
	let importPoll: ReturnType<typeof setInterval> | null = null;

	// A Spotify import fills its rows after the page opens; poll while it
	// runs so the list appears without a reload.
	$effect(() => {
		const running = playlist?.import_state === 'running';
		if (running && importPoll === null) {
			importPoll = setInterval(() => void detailQuery.refetch(), 3000);
		} else if (!running && importPoll !== null) {
			clearInterval(importPoll);
			importPoll = null;
		}
	});
	onDestroy(() => {
		if (importPoll !== null) clearInterval(importPoll);
	});

	let deleteModal = $state<ReturnType<typeof DeletePlaylistModal> | null>(null);
	let shareModal = $state<ReturnType<typeof SharePlaylistModal> | null>(null);
	let leaveOpen = $state(false);
	let trackList = $state<ReturnType<typeof PlaylistTrackList> | null>(null);
	let header = $state<ReturnType<typeof PlaylistHeader> | null>(null);

	let loading = $derived(detailQuery.isLoading);
	let locked = $derived(privatePlaylistStub(detailQuery.error));
	let loadError = $derived.by(() => {
		if (!detailQuery.isError) return null;
		const msg = detailQuery.error instanceof Error ? detailQuery.error.message : '';
		return /404|not found/i.test(msg) ? 'Playlist not found' : "Couldn't load this playlist";
	});

	let isOwner = $derived(playlist?.is_owner ?? false);
	let canDelete = $derived(playlist?.can_delete ?? false);

	type MissingItem =
		| { kind: 'album'; album: string; artist: string }
		| { kind: 'track'; title: string; artist: string };
	let missingItems = $derived.by(() => {
		if (!playlist) return [] as MissingItem[];
		const seen = new SvelteMap<string, MissingItem>();
		for (const t of playlist.tracks) {
			// The server groups the same way: owned rows are skipped, an
			// album row asks for its album, and an album-less row asks for
			// its recording.
			if (t.library_file_id) continue;
			const album = t.album_name.trim();
			const artist = t.artist_name.trim();
			const title = t.track_name.trim();
			if (!artist) continue;
			if (album) {
				const key = `a:${album.toLowerCase()}\u0000${artist.toLowerCase()}`;
				if (!seen.has(key)) seen.set(key, { kind: 'album', album, artist });
			} else if (title) {
				const key = `t:${title.toLowerCase()}\u0000${artist.toLowerCase()}`;
				if (!seen.has(key)) seen.set(key, { kind: 'track', title, artist });
			}
		}
		return [...seen.values()];
	});
	let missingAlbumCount = $derived(missingItems.length);

	let requesting = $state(false);
	let requestOpen = $state(false);
	let requestChoices = $state<Record<string, 'album' | 'track'>>({});
	let progress = $state<{ state: string; requested: number; failed: number; total: number } | null>(
		null
	);

	const requestRows = $derived(
		playlist
			? playlist.tracks.filter(
					(t) => !t.library_file_id && t.track_name.trim() !== '' && t.artist_name.trim() !== ''
				)
			: []
	);

	$effect(() => {
		if (!requestOpen) return;
		const next: Record<string, 'album' | 'track'> = { ...requestChoices };
		for (const row of requestRows) {
			if (!(row.id in next)) next[row.id] = row.album_name.trim() ? 'album' : 'track';
		}
		requestChoices = next;
	});

	async function handleRequestMissing() {
		if (requesting || !playlist) return;
		requesting = true;
		try {
			const seen = new SvelteSet<string>();
			const items = [];
			for (const row of requestRows) {
				const album = row.album_name.trim();
				const artist = row.artist_name.trim();
				const title = row.track_name.trim();
				if (!artist || !title) continue;
				const wantsAlbum = album.length > 0 && (requestChoices[row.id] ?? 'album') === 'album';
				if (wantsAlbum) {
					const key = `${album.toLowerCase()}\u0000${artist.toLowerCase()}`;
					if (seen.has(key)) continue;
					seen.add(key);
					items.push({ kind: 'album' as const, album, artist, title: null });
				} else {
					items.push({ kind: 'track' as const, album: null, artist, title });
				}
			}
			const state = await acquireMissing.mutateAsync({ id: playlist.id, items });
			progress = state;
			while (progress && progress.state === 'running') {
				await new Promise((resolve) => setTimeout(resolve, 1500));
				const next = await getAcquireProgressV3(playlist.id);
				if (next) progress = next;
			}
			const requested = progress?.requested ?? 0;
			const failed = progress?.failed ?? 0;
			toastStore.show({
				message: `${requested} requested${failed > 0 ? `, ${failed} unmatched` : ''}.`,
				type: requested > 0 ? 'success' : 'error'
			});
			requestOpen = false;
		} catch {
			toastStore.show({ message: "Couldn't submit requests", type: 'error' });
		} finally {
			requesting = false;
		}
	}

	function applySourcesMap(sources: Record<string, string[]>, onlyMissing = false) {
		if (!playlist) return;
		for (const track of playlist.tracks) {
			const resolved = sources[track.id];
			if (!resolved || resolved.length === 0) continue;
			if (onlyMissing) {
				// Live detailQuery values (clone) win: never let a stale cache entry
				// overwrite a healed row that already has sources or a local link.
				const hasLive =
					(track.available_sources && track.available_sources.length > 0) ||
					Boolean(track.library_file_id);
				if (hasLive) continue;
			}
			track.available_sources = resolved;
		}
	}

	function hasUsableSourcesForPlaylist(
		sources: Record<string, string[]>,
		trackIds: Set<string>
	): boolean {
		for (const [id, value] of Object.entries(sources)) {
			if (trackIds.has(id) && Array.isArray(value) && value.length > 0) return true;
		}
		return false;
	}

	async function resolveSources(playlistId: string) {
		const options = getPlaylistSourcesV3QueryOptions(authStore.user?.id, playlistId);
		const before = queryClient.getQueryData(options.queryKey);
		try {
			const sources = await queryClient.fetchQuery(options);
			if (!playlist || playlist.id !== playlistId) return;
			const ids = new Set(playlist.tracks.map((t) => t.id));
			if (!hasUsableSourcesForPlaylist(sources, ids)) {
				// Nothing usable is never fresh: the next visit asks again.
				await forgetSources(playlistId);
				return;
			}
			if (sources === before) {
				// Live detailQuery values (clone) already hold the healed rows;
				// the kept answer only fills gaps.
				applySourcesMap(sources, true);
				return;
			}
			applySourcesMap(sources);
			await refreshPlaylist(playlistId);
		} catch {
			// non-critical - tracks keep their stored available_sources
		}
	}

	async function forgetSources(playlistId: string) {
		try {
			await forgetPlaylistSources(playlistId);
		} catch {
			// the kept answer only goes stale on its own later
		}
	}

	let lastSyncedData: PlaylistDetailV3 | undefined;
	$effect(() => {
		const d = detailQuery.data;
		if (d === lastSyncedData) return;
		lastSyncedData = d;
		untrack(() => {
			trackList?.clearReorderState();
			header?.cleanupPreview();
			if (d) {
				// Clone so optimistic child mutations never touch the query cache.
				const page = toPageDetail(d);
				playlist = { ...page, tracks: page.tracks.map((t) => ({ ...t })) };
				void resolveSources(d.id);
			} else {
				playlist = null;
			}
		});
	});

	function playAll() {
		if (!playlist || playlist.tracks.length === 0) return;
		const items = playlist.tracks
			.map(playlistTrackToQueueItem)
			.filter((item): item is NonNullable<typeof item> => item !== null);
		if (items.length === 0) {
			toastStore.show({ message: 'Nothing here can be played right now', type: 'info' });
			return;
		}
		playerStore.playQueue(items, 0, false);
	}

	function shuffleAll() {
		if (!playlist || playlist.tracks.length < 2) return;
		const items = playlist.tracks
			.map(playlistTrackToQueueItem)
			.filter((item): item is NonNullable<typeof item> => item !== null);
		if (items.length === 0) {
			toastStore.show({ message: 'Nothing here can be played right now', type: 'info' });
			return;
		}
		playerStore.playQueue(items, 0, true);
	}

	function playFromTrack(index: number) {
		if (!playlist || playlist.tracks.length === 0) return;
		const rowItems = playlist.tracks.map(playlistTrackToQueueItem);
		if (!rowItems[index]) {
			toastStore.show({ message: 'Track is not playable', type: 'error' });
			return;
		}
		const items = rowItems.filter((item): item is NonNullable<typeof item> => item !== null);
		// Rows with no file are not queued, so count only the playable rows above this one.
		const startIndex = rowItems.slice(0, index).filter((item) => item !== null).length;
		playerStore.playQueue(items, startIndex, false);
	}

	function handleSourceChange() {
		if (playlist) void forgetSources(playlist.id);
	}

	function handlePlaylistUpdate(updatedPlaylist: PlaylistDetail) {
		playlist = updatedPlaylist;
	}

	// A failure throws, so the dialog stays open and shows it.
	async function leave() {
		if (!playlist) return;
		await leaveMutation.mutateAsync(playlist.id);
		toastStore.show({ message: 'You left the playlist', type: 'success' });
		await goto(withBasePath('/playlists'));
	}

	async function confirmDelete() {
		if (!playlist || deleting) return;
		deleting = true;
		try {
			await deleteMutation.mutateAsync(playlist.id);
			toastStore.show({ message: 'Playlist deleted', type: 'success' });
			await goto(withBasePath('/playlists'));
		} catch {
			toastStore.show({ message: "Couldn't delete the playlist", type: 'error' });
		} finally {
			deleting = false;
		}
	}

	onDestroy(() => {
		trackList?.clearReorderState();
		header?.cleanupPreview();
	});
</script>

<svelte:head>
	<title>{locked ? 'Private playlist' : (playlist?.name ?? 'Playlist')} - DroppedNeedle</title>
</svelte:head>

<div class="w-full">
	{#if loading}
		<div class="mx-auto max-w-7xl space-y-8 px-4 py-5 sm:px-6 lg:px-8" aria-busy="true">
			<div class="skeleton size-12 rounded-full"></div>
			<div class="flex flex-col gap-8 md:flex-row md:items-center">
				<div class="skeleton aspect-square w-full max-w-72 shrink-0 rounded-md lg:max-w-80"></div>
				<div class="flex flex-1 flex-col gap-4">
					<div class="skeleton h-16 w-3/4"></div>
					<div class="skeleton h-4 w-48"></div>
					<div class="skeleton h-4 w-64"></div>
					<div class="mt-4 flex gap-3">
						<div class="skeleton h-12 w-36"></div>
						<div class="skeleton h-12 w-32"></div>
					</div>
				</div>
			</div>
			<div class="space-y-2">
				{#each Array(4) as _, i (`loading-track-${i}`)}
					<div class="skeleton h-14 w-full"></div>
				{/each}
			</div>
		</div>
	{:else if locked}
		<PlaylistNotice icon={Lock} title="Private playlist" message={privatePlaylistLine(locked)} />
	{:else if loadError}
		<PlaylistNotice
			icon={Music}
			title="Couldn't load this playlist"
			message={loadError}
			onretry={() => void detailQuery.refetch()}
		/>
	{:else if !playlist}
		<PlaylistNotice icon={Music} title="Playlist not found" />
	{:else}
		<PlaylistHeader
			bind:this={header}
			{playlist}
			canEdit={isOwner}
			{canDelete}
			onplayall={playAll}
			onshuffleall={shuffleAll}
			ondeleteclick={() => deleteModal?.showModal()}
			onplaylistupdate={handlePlaylistUpdate}
			onshare={() => shareModal?.showModal()}
			onleave={() => (leaveOpen = true)}
		/>

		<div class="mx-auto max-w-7xl space-y-6 px-4 pb-10 sm:px-6 lg:px-8">
			{#if playlist?.import_state === 'running'}
				<div class="flex items-center gap-3 rounded-box border border-line px-4 py-3 text-sm">
					<Music class="size-4 shrink-0 animate-pulse" aria-hidden="true" />
					<p>Importing this playlist from Spotify. The tracks appear here as they arrive.</p>
				</div>
			{:else if playlist?.import_state === 'failed'}
				<div
					class="flex items-center gap-3 rounded-box border border-warning/60 bg-warning/8 px-4 py-3 text-sm"
				>
					<Music class="size-4 shrink-0 text-warning" aria-hidden="true" />
					<p>The Spotify import failed for this playlist. Import it again to retry.</p>
				</div>
			{/if}
			{#if isOwner && missingAlbumCount > 0}
				<div
					class="flex flex-wrap items-center gap-x-4 gap-y-3 rounded-box border border-warning/60 bg-warning/8 px-4 py-3"
				>
					<Download class="size-5 shrink-0 text-warning" aria-hidden="true" />
					<p class="min-w-0 flex-1 basis-48 text-sm font-medium">
						{missingAlbumCount} missing from your library
					</p>
					<Button
						variant="outline"
						size="display-sm"
						icon={Download}
						loading={requesting}
						onclick={() => (requestOpen = true)}
					>
						Request {missingAlbumCount}
					</Button>
				</div>
			{/if}

			<PlaylistTrackList
				bind:this={trackList}
				{playlist}
				readonly={!playlist.can_edit}
				ontrackchange={() => {}}
				onsourcechange={handleSourceChange}
				onplaytrack={playFromTrack}
			/>
		</div>

		{#if isOwner}
			<Modal
				bind:open={requestOpen}
				title="Request missing songs"
				description="Choose what to ask for, album or single track"
				size="lg"
				closeButton
			>
				<div class="flex max-h-[60vh] flex-col gap-2 overflow-y-auto">
					{#each requestRows as row (row.id)}
						<div class="flex items-center justify-between gap-3 text-sm">
							<span class="min-w-0 truncate">
								<span class="font-medium">{row.track_name}</span>
								<span class="text-muted">by {row.artist_name}</span>
							</span>
							{#if row.album_name.trim()}
								<span class="flex shrink-0 items-center gap-3">
									<label class="inline-flex items-center gap-1">
										<input type="radio" value="album" bind:group={requestChoices[row.id]} />
										Album
									</label>
									<label class="inline-flex items-center gap-1">
										<input type="radio" value="track" bind:group={requestChoices[row.id]} />
										Track
									</label>
								</span>
							{:else}
								<span class="shrink-0 text-xs text-muted">single track</span>
							{/if}
						</div>
					{/each}
				</div>
				{#snippet actions()}
					<Button variant="ghost" onclick={() => (requestOpen = false)}>Cancel</Button>
					<Button
						variant="primary"
						loading={requesting}
						onclick={() => void handleRequestMissing()}
					>
						Request {requestRows.length}
					</Button>
				{/snippet}
			</Modal>
			<SharePlaylistModal
				bind:this={shareModal}
				playlistId={playlist.id}
				isPublic={playlist.is_public}
				onsaved={(isPublic) => {
					if (playlist) playlist = { ...playlist, is_public: isPublic };
				}}
			/>
		{/if}

		{#if !isOwner}
			<ConfirmDialog
				bind:open={leaveOpen}
				title="Leave playlist"
				icon={LogOut}
				tone="warning"
				confirmLabel="Leave"
				pending={leaveMutation.isPending}
				message="You'll stop seeing {playlist.name} until {playlist.owner_name ??
					'its owner'} shares it with you again."
				onconfirm={leave}
			/>
		{/if}

		<DeletePlaylistModal
			bind:this={deleteModal}
			playlistName={playlist.name}
			{deleting}
			onconfirm={confirmDelete}
		/>
	{/if}
</div>
