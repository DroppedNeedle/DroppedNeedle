<script lang="ts">
	import { goto } from '$app/navigation';
	import { onDestroy, untrack } from 'svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import { requestMissingTracks, type PlaylistDetail } from '$lib/api/playlists';
	import { playlistTrackToQueueItem } from '$lib/features/player/playback/queueHelpers';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		getPlaylistDetailV3Query,
		getPlaylistSourcesV3QueryOptions,
		type PlaylistDetailV3
	} from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
	import {
		createDeletePlaylistV3Mutation,
		forgetPlaylistSources,
		refreshPlaylist,
		createSetPlaylistVisibilityV3Mutation
	} from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import {
		privatePlaylistLine,
		privatePlaylistStub,
		toPageDetail
	} from '$lib/features/playlists/queries/playlistV3Adapter';
	import { queryClient } from '$lib/queries/QueryClient';
	import { withBasePath } from '$lib/utils/basePath';
	import { Music, Download, Lock } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import type { PageData } from './$types';
	import PlaylistHeader from '$lib/features/playlists/components/PlaylistHeader.svelte';
	import PlaylistTrackList from '$lib/features/playlists/components/PlaylistTrackList.svelte';
	import DeletePlaylistModal from '$lib/features/playlists/components/DeletePlaylistModal.svelte';
	import PlaylistNotice from '$lib/features/playlists/components/PlaylistNotice.svelte';

	let { data }: { data: PageData } = $props();

	const detailQuery = getPlaylistDetailV3Query(
		() => data.playlistId,
		() => true
	);
	const shareMutation = createSetPlaylistVisibilityV3Mutation();
	const deleteMutation = createDeletePlaylistV3Mutation();

	// A local mutable copy of the (full) playlist so child components can keep
	// applying optimistic updates; redaction/loading/error leave it null.
	let playlist = $state<PlaylistDetail | null>(null);
	let deleting = $state(false);

	let deleteModal = $state<ReturnType<typeof DeletePlaylistModal> | null>(null);
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

	let missingAlbumCount = $derived.by(() => {
		if (!playlist) return 0;
		const seen = new SvelteSet<string>();
		for (const t of playlist.tracks) {
			// library_file_id means owned locally (request-missing skips these too).
			if (
				t.album_id &&
				!t.library_file_id &&
				(!t.available_sources || t.available_sources.length === 0)
			) {
				seen.add(t.album_id);
			}
		}
		return seen.size;
	});

	let requesting = $state(false);

	async function handleRequestMissing() {
		if (requesting || !playlist) return;
		requesting = true;
		try {
			const result = await requestMissingTracks(playlist.tracks);
			toastStore.show({ message: result.message, type: 'success' });
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

	async function handleShare(isPublic: boolean) {
		if (!playlist || shareMutation.isPending) return;
		try {
			const updated = await shareMutation.mutateAsync({ id: playlist.id, isPublic });
			playlist = { ...playlist, is_public: updated.is_public };
			toastStore.show({
				message: updated.is_public ? 'Playlist is now public' : 'Playlist is now private',
				type: 'success'
			});
		} catch {
			toastStore.show({ message: "Couldn't update sharing", type: 'error' });
		}
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
			sharePending={shareMutation.isPending}
			onplayall={playAll}
			onshuffleall={shuffleAll}
			ondeleteclick={() => deleteModal?.showModal()}
			onplaylistupdate={handlePlaylistUpdate}
			onshare={handleShare}
		/>

		<div class="mx-auto max-w-7xl space-y-6 px-4 pb-10 sm:px-6 lg:px-8">
			{#if isOwner && missingAlbumCount > 0}
				<div
					class="flex flex-wrap items-center gap-x-4 gap-y-3 rounded-box border border-warning/60 bg-warning/8 px-4 py-3"
				>
					<Download class="size-5 shrink-0 text-warning" aria-hidden="true" />
					<p class="min-w-0 flex-1 basis-48 text-sm font-medium">
						{missingAlbumCount}
						{missingAlbumCount === 1 ? 'album' : 'albums'} not in your library
					</p>
					<Button
						variant="outline"
						size="display-sm"
						icon={Download}
						loading={requesting}
						onclick={() => void handleRequestMissing()}
					>
						Request {missingAlbumCount === 1 ? 'album' : missingAlbumCount + ' albums'}
					</Button>
				</div>
			{/if}

			<PlaylistTrackList
				bind:this={trackList}
				{playlist}
				readonly={!isOwner}
				ontrackchange={() => {}}
				onsourcechange={handleSourceChange}
				onplaytrack={playFromTrack}
			/>
		</div>

		<DeletePlaylistModal
			bind:this={deleteModal}
			playlistName={playlist.name}
			{deleting}
			onconfirm={confirmDelete}
		/>
	{/if}
</div>
