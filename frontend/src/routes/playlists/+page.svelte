<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import {
		isRedactedPlaylist,
		type PlaylistListItem,
		type PlaylistSummary,
		type RedactedPlaylist
	} from '$lib/api/playlists';
	import { toastStore } from '$lib/stores/toast';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getConnectionsQuery } from '$lib/queries/connections/ConnectionsQuery.svelte';
	import { getPlaylistListQuery } from '$lib/queries/playlists/PlaylistQuery.svelte';
	import { createCreatePlaylistMutation } from '$lib/queries/playlists/PlaylistMutations.svelte';
	import { ListMusic, Plus, Lock, Sparkles, Users } from 'lucide-svelte';
	import PageHero from '$lib/ui/PageHero.svelte';
	import SmartMixModal from '$lib/components/SmartMixModal.svelte';
	import SpotifyIcon from '$lib/components/SpotifyIcon.svelte';
	import PlaylistCard from '$lib/components/PlaylistCard.svelte';
	import RedactedPlaylistCard from '$lib/components/RedactedPlaylistCard.svelte';
	import PlaylistCardSkeleton from '$lib/components/PlaylistCardSkeleton.svelte';

	const connectionsQuery = getConnectionsQuery();
	const spotifyLinked = $derived(
		connectionsQuery.data?.connections.some((c) => c.service === 'spotify') ?? false
	);

	const query = getPlaylistListQuery(() => authStore.isAuthenticated);
	const createMutation = createCreatePlaylistMutation();

	let items = $derived((query.data ?? []) as PlaylistListItem[]);
	let myPlaylists = $derived(
		items.filter((p): p is PlaylistSummary => !isRedactedPlaylist(p) && p.is_owner)
	);
	let sharedPlaylists = $derived(
		items.filter((p): p is PlaylistSummary => !isRedactedPlaylist(p) && !p.is_owner)
	);
	let redactedPlaylists = $derived(items.filter(isRedactedPlaylist) as RedactedPlaylist[]);
	let errorMessage = $derived(
		query.isError
			? query.error instanceof Error
				? query.error.message
				: "Couldn't load playlists"
			: null
	);

	let showNewInput = $state(false);
	let newName = $state('');
	let newNameInputEl = $state<HTMLInputElement | null>(null);
	let smartMixModal = $state<SmartMixModal | null>(null);

	$effect(() => {
		if (showNewInput && newNameInputEl) {
			newNameInputEl.focus();
		}
	});

	// the sidebar's "New playlist" pill deep-links here with ?new=1
	$effect(() => {
		if (page.url.searchParams.get('new') === '1') {
			showNewInput = true;
		}
	});

	const gridClass =
		'grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4';

	async function handleCreate() {
		const trimmed = newName.trim();
		if (!trimmed || createMutation.isPending) return;
		try {
			const created = await createMutation.mutateAsync(trimmed);
			newName = '';
			showNewInput = false;
			await goto(`/playlists/${created.id}`);
		} catch (_e) {
			toastStore.show({ message: "Couldn't create the playlist", type: 'error' });
		}
	}

	function handleCreateKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') void handleCreate();
		if (e.key === 'Escape') {
			showNewInput = false;
			newName = '';
		}
	}

	function handleCardDelete() {
		void query.refetch();
	}
</script>

<svelte:head>
	<title>Playlists - DroppedNeedle</title>
</svelte:head>

<PageHero title="Playlists" subtitle="Your mixes, moods, and moments." eyebrow="Your music">
	{#snippet icon()}
		<ListMusic class="h-7 w-7" />
	{/snippet}
	{#snippet actions()}
		<button
			class="btn btn-primary btn-sm gap-1.5 rounded-full"
			onclick={() => {
				showNewInput = true;
			}}
		>
			<Plus class="h-4 w-4" />
			New Playlist
		</button>
		<button
			class="btn btn-ghost btn-sm gap-1.5 rounded-full bg-base-content/6"
			onclick={() => smartMixModal?.showModal()}
		>
			<Sparkles class="h-3.5 w-3.5 text-accent" />
			Smart Mix
		</button>
		{#if spotifyLinked}
			<a
				href="/playlists/spotify"
				class="btn btn-ghost btn-sm gap-1.5 rounded-full bg-base-content/6"
			>
				<SpotifyIcon class="h-3.5 w-3.5 text-accent" />
				Import from Spotify
			</a>
		{/if}
	{/snippet}
</PageHero>

<div class="space-y-10 px-4 pb-12 sm:px-6 lg:px-8">
	{#if showNewInput}
		<div
			class="flex items-center gap-2 rounded-2xl border border-base-content/8 bg-base-200/50 p-3"
		>
			<input
				type="text"
				class="input input-sm flex-1 rounded-full"
				placeholder="Playlist name..."
				bind:this={newNameInputEl}
				bind:value={newName}
				onkeydown={handleCreateKeydown}
			/>
			<button
				class="btn btn-primary btn-sm rounded-full"
				onclick={() => void handleCreate()}
				disabled={!newName.trim() || createMutation.isPending}
			>
				{#if createMutation.isPending}
					<span class="loading loading-spinner loading-xs"></span>
				{:else}
					Create
				{/if}
			</button>
			<button
				class="btn btn-ghost btn-sm rounded-full"
				onclick={() => {
					showNewInput = false;
					newName = '';
				}}
			>
				Cancel
			</button>
		</div>
	{/if}

	{#if query.isLoading}
		<div class={gridClass}>
			{#each Array(8) as _, i (`skeleton-${i}`)}
				<PlaylistCardSkeleton />
			{/each}
		</div>
	{:else if errorMessage}
		<div role="alert" class="alert alert-error">
			<span>{errorMessage}</span>
			<button class="btn btn-sm btn-ghost" onclick={() => void query.refetch()}>Retry</button>
		</div>
	{:else if items.length === 0}
		<div
			class="flex flex-col items-center justify-center gap-4 rounded-2xl border border-dashed border-base-content/12 py-20"
		>
			<ListMusic class="h-16 w-16 text-base-content/20" />
			<h2 class="font-display text-lg font-semibold tracking-tight text-base-content/60">
				No playlists yet
			</h2>
			<button
				class="btn btn-primary btn-sm rounded-full"
				onclick={() => {
					showNewInput = true;
				}}
			>
				<Plus class="h-4 w-4" />
				Create your first playlist
			</button>
		</div>
	{:else}
		{#if myPlaylists.length > 0}
			<section class="space-y-4" aria-label="My playlists">
				<h2
					class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
				>
					<ListMusic class="h-4 w-4 text-accent" />
					My Playlists
				</h2>
				<div class={gridClass}>
					{#each myPlaylists as playlist (playlist.id)}
						<PlaylistCard {playlist} ondelete={handleCardDelete} />
					{/each}
				</div>
			</section>
		{/if}

		{#if sharedPlaylists.length > 0}
			<section class="space-y-4" aria-label="Shared with you">
				<h2
					class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
				>
					<Users class="h-4 w-4 text-accent" />
					Shared with you
				</h2>
				<div class={gridClass}>
					{#each sharedPlaylists as playlist (playlist.id)}
						<PlaylistCard {playlist} ondelete={handleCardDelete} />
					{/each}
				</div>
			</section>
		{/if}

		{#if redactedPlaylists.length > 0}
			<section class="space-y-4" aria-label="Private playlists, admin view">
				<h2
					class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/40"
				>
					<Lock class="h-4 w-4 text-accent" />
					Private &middot; admin view
				</h2>
				<div class={gridClass}>
					{#each redactedPlaylists as playlist (playlist.id)}
						<RedactedPlaylistCard {playlist} />
					{/each}
				</div>
			</section>
		{/if}
	{/if}
</div>

<SmartMixModal bind:this={smartMixModal} />
