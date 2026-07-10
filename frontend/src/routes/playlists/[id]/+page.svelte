<script lang="ts">
	import { goto } from '$app/navigation';
	import { onDestroy, untrack } from 'svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import {
		deletePlaylist,
		resolvePlaylistSources,
		requestMissingTracks,
		isRedactedPlaylist,
		type PlaylistDetail,
		type PlaylistDetailItem,
		type RedactedPlaylist
	} from '$lib/api/playlists';
	import { playlistTrackToQueueItem } from '$lib/player/queueHelpers';
	import { playerStore } from '$lib/stores/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getCacheTTL } from '$lib/stores/cacheTtl';
	import { getPlaylistDetailQuery } from '$lib/queries/playlists/PlaylistQuery.svelte';
	import { createSetPlaylistPublicMutation } from '$lib/queries/playlists/PlaylistMutations.svelte';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { PlaylistQueryKeyFactory } from '$lib/queries/playlists/PlaylistQueryKeyFactory';
	import { extractDominantColor, DEFAULT_GRADIENT } from '$lib/utils/colors';
	import { getApiUrl } from '$lib/api/api-utils';
	import { Music, Lock, Download, Loader2 } from 'lucide-svelte';
	import BackButton from '$lib/components/BackButton.svelte';
	import type { PageData } from './$types';
	import PlaylistHeader from './PlaylistHeader.svelte';
	import PlaylistTrackList from './PlaylistTrackList.svelte';
	import DeletePlaylistModal from './DeletePlaylistModal.svelte';

	let { data }: { data: PageData } = $props();

	const detailQuery = getPlaylistDetailQuery(
		() => data.playlistId,
		() => true
	);
	const shareMutation = createSetPlaylistPublicMutation();

	// A local mutable copy of the (full) playlist so child components can keep
	// applying optimistic updates; redaction/loading/error leave it null.
	let playlist = $state<PlaylistDetail | null>(null);
	let deleting = $state(false);

	let deleteModal = $state<ReturnType<typeof DeletePlaylistModal> | null>(null);
	let trackList = $state<ReturnType<typeof PlaylistTrackList> | null>(null);
	let header = $state<ReturnType<typeof PlaylistHeader> | null>(null);

	let redacted = $derived(
		detailQuery.data && isRedactedPlaylist(detailQuery.data)
			? (detailQuery.data as RedactedPlaylist)
			: null
	);
	let loading = $derived(detailQuery.isLoading);
	let loadError = $derived.by(() => {
		if (!detailQuery.isError) return null;
		const msg = detailQuery.error instanceof Error ? detailQuery.error.message : '';
		return /404|not found/i.test(msg) ? 'Playlist not found' : "Couldn't load this playlist";
	});

	let isOwner = $derived(playlist?.is_owner ?? false);
	let canDelete = $derived((playlist?.is_owner ?? false) || authStore.isAdmin);

	let missingAlbumCount = $derived.by(() => {
		if (!playlist) return 0;
		const seen = new SvelteSet<string>();
		for (const t of playlist.tracks) {
			if (t.album_id && (!t.available_sources || t.available_sources.length === 0)) {
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
			const result = await requestMissingTracks(playlist.id);
			toastStore.show({ message: result.message, type: 'success' });
		} catch {
			toastStore.show({ message: "Couldn't submit requests", type: 'error' });
		} finally {
			requesting = false;
		}
	}

	// Source-resolution cache is namespaced per user so two accounts on a shared
	// browser never read each other's resolved sources (AMU-5).
	const SOURCES_CACHE_PREFIX = 'droppedneedle_playlist_sources_';
	function sourcesCacheKey(playlistId: string): string {
		return `${SOURCES_CACHE_PREFIX}${authStore.user?.id ?? 'anon'}_${playlistId}`;
	}

	function getSourcesFromCache(playlistId: string): Record<string, string[]> | null {
		try {
			const key = sourcesCacheKey(playlistId);
			const raw = localStorage.getItem(key);
			if (!raw) return null;
			const cached = JSON.parse(raw) as { ts: number; data: Record<string, string[]> };
			const ttl = getCacheTTL('playlistSources');
			if (Date.now() - cached.ts > ttl) {
				localStorage.removeItem(key);
				return null;
			}
			return cached.data;
		} catch {
			return null;
		}
	}

	function setSourcesCache(playlistId: string, sources: Record<string, string[]>) {
		try {
			localStorage.setItem(
				sourcesCacheKey(playlistId),
				JSON.stringify({ ts: Date.now(), data: sources })
			);
		} catch {
			/* storage full - non-critical */
		}
	}

	function invalidateSourcesCache(playlistId: string) {
		try {
			localStorage.removeItem(sourcesCacheKey(playlistId));
		} catch {
			/* ignore */
		}
	}

	function applySourcesMap(sources: Record<string, string[]>) {
		if (!playlist) return;
		for (const track of playlist.tracks) {
			const resolved = sources[track.id];
			if (resolved && resolved.length > 0) {
				track.available_sources = resolved;
			}
		}
	}

	async function resolveAndCacheSources(playlistId: string) {
		const cached = getSourcesFromCache(playlistId);
		if (cached) {
			applySourcesMap(cached);
			return;
		}
		try {
			const sources = await resolvePlaylistSources(playlistId);
			if (playlist && playlist.id === playlistId) {
				applySourcesMap(sources);
				setSourcesCache(playlistId, sources);
			}
		} catch {
			// non-critical - tracks keep their stored available_sources
		}
	}

	let lastSyncedData: PlaylistDetailItem | undefined;
	$effect(() => {
		const d = detailQuery.data;
		if (d === lastSyncedData) return;
		lastSyncedData = d;
		untrack(() => {
			trackList?.clearReorderState();
			header?.cleanupPreview();
			if (d && !isRedactedPlaylist(d)) {
				// Clone so optimistic child mutations never touch the query cache.
				playlist = { ...d, tracks: d.tracks.map((t) => ({ ...t })) };
				void resolveAndCacheSources(d.id);
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
		const items = playlist.tracks
			.map(playlistTrackToQueueItem)
			.filter((item): item is NonNullable<typeof item> => item !== null);
		if (items.length === 0) {
			toastStore.show({ message: 'Nothing here can be played right now', type: 'info' });
			return;
		}
		const startIndex = Math.min(index, items.length - 1);
		playerStore.playQueue(items, startIndex, false);
	}

	function handleSourceChange() {
		if (playlist) invalidateSourcesCache(playlist.id);
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
			await deletePlaylist(playlist.id);
			await invalidateQueriesWithPersister({
				queryKey: PlaylistQueryKeyFactory.list(authStore.user?.id)
			});
			toastStore.show({ message: 'Playlist deleted', type: 'success' });
			await goto('/playlists');
		} catch {
			toastStore.show({ message: "Couldn't delete the playlist", type: 'error' });
		} finally {
			deleting = false;
		}
	}

	let heroGradient = $state(DEFAULT_GRADIENT);

	let heroBgUrl = $derived.by(() => {
		if (!playlist) return null;
		if (playlist.custom_cover_url) return getApiUrl(playlist.custom_cover_url);
		if (playlist.cover_urls.length > 0) return getApiUrl(playlist.cover_urls[0]);
		return null;
	});

	$effect(() => {
		const url = heroBgUrl;
		if (url) {
			extractDominantColor(url).then((gradient) => (heroGradient = gradient));
		} else {
			heroGradient = DEFAULT_GRADIENT;
		}
	});

	onDestroy(() => {
		trackList?.clearReorderState();
		header?.cleanupPreview();
	});
</script>

<svelte:head>
	<title>{playlist?.name ?? 'Playlist'} - DroppedNeedle</title>
</svelte:head>

{#if loading}
	<div class="mx-auto w-full max-w-7xl px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
		<div class="space-y-6 sm:space-y-8">
			<div class="skeleton h-10 w-10 rounded-full"></div>
			<div class="flex flex-col gap-6 lg:flex-row lg:gap-8">
				<div class="skeleton aspect-square w-full shrink-0 rounded-2xl lg:w-64 xl:w-80"></div>
				<div class="flex flex-1 flex-col justify-end space-y-4">
					<div class="skeleton h-4 w-20"></div>
					<div class="skeleton h-12 w-3/4"></div>
					<div class="skeleton h-6 w-1/2"></div>
					<div class="mt-6 flex gap-4">
						<div class="skeleton h-12 w-32 rounded-full"></div>
						<div class="skeleton h-12 w-32 rounded-full"></div>
					</div>
				</div>
			</div>
			<div class="space-y-2">
				{#each Array(4) as _, i (`loading-track-${i}`)}
					<div class="skeleton h-14 w-full rounded-xl"></div>
				{/each}
			</div>
		</div>
	</div>
{:else if loadError}
	<div class="mx-auto w-full max-w-7xl px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
		<div class="flex flex-col items-center justify-center gap-4 py-20 text-center">
			<Music class="h-16 w-16 text-base-content/20" />
			<h2 class="font-display text-lg font-semibold tracking-tight text-base-content/80">
				Couldn't load this playlist
			</h2>
			<p class="text-sm text-base-content/60">{loadError}</p>
			<div class="flex items-center gap-2">
				<button
					class="btn btn-primary btn-sm rounded-full"
					onclick={() => void detailQuery.refetch()}
				>
					Retry
				</button>
				<BackButton fallback="/playlists" />
			</div>
		</div>
	</div>
{:else if redacted}
	<div class="mx-auto w-full max-w-7xl px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
		<div class="flex flex-col items-center justify-center gap-4 py-20 text-center">
			<div
				class="flex items-center justify-center rounded-full border border-base-content/8 bg-base-200/60 p-5"
			>
				<Lock class="h-12 w-12 text-base-content/30" />
			</div>
			<h2 class="font-display text-lg font-semibold italic text-base-content/70">
				Private playlist
			</h2>
			<p class="text-sm text-base-content/60">
				{redacted.track_count} track{redacted.track_count === 1 ? '' : 's'}{redacted.owner_name
					? ` · owned by ${redacted.owner_name}`
					: ''}
			</p>
			<BackButton fallback="/playlists" />
		</div>
	</div>
{:else if !playlist}
	<div class="mx-auto w-full max-w-7xl px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
		<div class="flex flex-col items-center justify-center gap-4 py-20">
			<Music class="h-16 w-16 text-base-content/20" />
			<h2 class="font-display text-lg font-semibold tracking-tight text-base-content/60">
				Playlist not found
			</h2>
			<BackButton fallback="/playlists" />
		</div>
	</div>
{:else}
	<!-- cinematic full-bleed masthead: cover backdrop under a charcoal wash -->
	<section class="dn-playlist-hero" aria-label="Playlist details">
		<div class="dn-playlist-hero__backdrop" aria-hidden="true">
			<div
				class="absolute inset-0 bg-linear-to-b {heroGradient} transition-all duration-1000"
			></div>
			{#if heroBgUrl}
				{#key heroBgUrl}
					<img src={heroBgUrl} alt="" />
				{/key}
			{/if}
			<div class="dn-playlist-hero__wash"></div>
		</div>

		<div class="dn-playlist-hero__content mx-auto w-full max-w-7xl px-4 sm:px-6 lg:px-8">
			<div class="mb-6">
				<BackButton fallback="/playlists" />
			</div>

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
		</div>
	</section>

	<div class="mx-auto w-full max-w-7xl space-y-6 px-2 pb-12 pt-6 sm:space-y-8 sm:px-4 lg:px-8">
		{#if isOwner && missingAlbumCount > 0}
			<div
				class="flex items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 px-4 py-3"
			>
				<Download class="h-4 w-4 shrink-0 text-base-content/50" />
				<p class="flex-1 text-sm text-base-content/70">
					{missingAlbumCount}
					{missingAlbumCount === 1 ? 'album' : 'albums'} not in your library
				</p>
				<button
					class="btn btn-accent btn-sm rounded-full"
					onclick={() => void handleRequestMissing()}
					disabled={requesting}
				>
					{#if requesting}
						<Loader2 class="h-3.5 w-3.5 animate-spin" />
					{:else}
						<Download class="h-3.5 w-3.5" />
					{/if}
					Request {missingAlbumCount === 1 ? 'album' : missingAlbumCount + ' albums'}
				</button>
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
		onconfirm={() => void confirmDelete()}
	/>
{/if}

<style>
	.dn-playlist-hero {
		position: relative;
		isolation: isolate;
		overflow: hidden;
		display: flex;
		flex-direction: column;
		justify-content: flex-end;
		min-height: min(50vh, 30rem);
		padding: 2.5rem 0 2.75rem;
	}
	@media (min-width: 1024px) {
		.dn-playlist-hero {
			padding: 3.25rem 0 3.25rem;
		}
	}

	.dn-playlist-hero__backdrop {
		position: absolute;
		inset: 0;
		z-index: -1;
	}
	.dn-playlist-hero__backdrop img {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: cover;
		object-position: center 30%;
		transform: scale(1.08);
		filter: saturate(0.9) brightness(0.7) blur(22px);
		transition: opacity var(--dn-dur-slow, 400ms) ease;
	}
	.dn-playlist-hero__wash {
		position: absolute;
		inset: 0;
		background:
			linear-gradient(
				90deg,
				oklch(from var(--color-base-100) l c h / 0.9) 0%,
				oklch(from var(--color-base-100) l c h / 0.55) 45%,
				oklch(from var(--color-base-100) l c h / 0.25) 100%
			),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-100) l c h / 0.4) 0%,
				oklch(from var(--color-base-100) l c h / 0.15) 40%,
				var(--color-base-100) 100%
			);
	}

	.dn-playlist-hero__content {
		position: relative;
		z-index: 1;
	}
</style>
