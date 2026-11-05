<script lang="ts">
	import { goto } from '$app/navigation';
	import {
		isRedactedPlaylist,
		type PlaylistSummary,
		type RedactedPlaylist
	} from '$lib/api/playlists';
	import { toastStore } from '$lib/stores/toast';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getConnectionsQuery } from '$lib/features/profile/queries/ConnectionsQuery.svelte';
	import { getPlaylistListV3Query } from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
	import { createPlaylistV3Mutation } from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import { toPageList } from '$lib/features/playlists/queries/playlistV3Adapter';
	import { withBasePath } from '$lib/utils/basePath';
	import { Plus, Lock, TriangleAlert, RefreshCw } from 'lucide-svelte';
	import SpotifyIcon from '$lib/media/icons/SpotifyIcon.svelte';
	import PlaylistCard from '$lib/features/playlists/components/PlaylistCard.svelte';
	import RedactedPlaylistCard from '$lib/features/playlists/components/RedactedPlaylistCard.svelte';
	import PlaylistCardSkeleton from '$lib/features/playlists/components/PlaylistCardSkeleton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Input from '$lib/ui/Input.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';

	const connectionsQuery = getConnectionsQuery();
	const spotifyLinked = $derived(
		connectionsQuery.data?.connections.some((c) => c.service === 'spotify') ?? false
	);

	const query = getPlaylistListV3Query(() => authStore.isAuthenticated);
	const createMutation = createPlaylistV3Mutation();

	let items = $derived(toPageList(query.data ?? []));
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

	$effect(() => {
		if (showNewInput && newNameInputEl) {
			newNameInputEl.focus();
		}
	});

	function cancelCreate() {
		showNewInput = false;
		newName = '';
	}

	async function handleCreate() {
		const trimmed = newName.trim();
		if (!trimmed || createMutation.isPending) return;
		try {
			const created = await createMutation.mutateAsync(trimmed);
			newName = '';
			showNewInput = false;
			await goto(withBasePath(`/playlists/${created.id}`));
		} catch (_e) {
			toastStore.show({ message: "Couldn't create the playlist", type: 'error' });
		}
	}

	function handleCreateKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') void handleCreate();
		if (e.key === 'Escape') cancelCreate();
	}

	function handleCardDelete() {
		void query.refetch();
	}
</script>

<svelte:head>
	<title>Playlists - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<ListPageHeader title="Playlists" subtitle="Made by you and shared with you" ticket="DN / CC 3.2">
		{#snippet below()}
			<div class="pl-actions">
				<Button variant="primary" size="display" icon={Plus} onclick={() => (showNewInput = true)}>
					New Playlist
				</Button>
				{#if spotifyLinked}
					<Button variant="outline" size="display" href={withBasePath('/playlists/spotify')}>
						<SpotifyIcon class="size-5 text-spotify" />
						Import from Spotify
					</Button>
				{/if}
			</div>
		{/snippet}
	</ListPageHeader>

	<div class="space-y-9 px-4 pb-10 sm:px-6 lg:px-8">
		{#if showNewInput}
			<div class="pl-new">
				<Input
					label="Playlist name"
					placeholder="Playlist name..."
					class="min-w-0 flex-1"
					bind:ref={newNameInputEl}
					bind:value={newName}
					onkeydown={handleCreateKeydown}
				/>
				<div class="flex gap-2">
					<Button
						variant="primary"
						size="display-sm"
						loading={createMutation.isPending}
						disabled={!newName.trim()}
						onclick={() => void handleCreate()}
					>
						Create
					</Button>
					<Button variant="ghost" size="display-sm" onclick={cancelCreate}>Cancel</Button>
				</div>
			</div>
		{/if}

		{#if query.isLoading}
			<div class="pl-grid" aria-busy="true">
				{#each Array(8) as _, i (`skeleton-${i}`)}
					<PlaylistCardSkeleton />
				{/each}
			</div>
		{:else if errorMessage}
			<div
				role="alert"
				class="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-box border border-error/60 bg-error/8 px-4 py-3"
			>
				<TriangleAlert class="size-5 shrink-0 text-error" aria-hidden="true" />
				<p class="min-w-0 flex-1 basis-56 text-sm">{errorMessage}</p>
				<Button
					variant="outline"
					size="display-sm"
					icon={RefreshCw}
					onclick={() => void query.refetch()}
				>
					Retry
				</Button>
			</div>
		{:else}
			<section>
				<SectionHead title="My Playlists" />
				<div class="pl-grid">
					{#each myPlaylists as playlist (playlist.id)}
						<PlaylistCard {playlist} ondelete={handleCardDelete} />
					{:else}
						<button type="button" class="pl-create" onclick={() => (showNewInput = true)}>
							<span class="pl-create__plus" aria-hidden="true"><Plus class="size-6" /></span>
							<span class="min-w-0">
								<span class="pl-create__title">Create your first playlist</span>
								<span class="pl-create__sub">No playlists yet</span>
							</span>
						</button>
					{/each}
				</div>
			</section>

			{#if sharedPlaylists.length > 0}
				<section>
					<SectionHead title="Shared with you" />
					<div class="pl-grid">
						{#each sharedPlaylists as playlist (playlist.id)}
							<PlaylistCard {playlist} ondelete={handleCardDelete} />
						{/each}
					</div>
				</section>
			{/if}

			{#if redactedPlaylists.length > 0}
				<section>
					<SectionHead title="Private" icon={Lock} />
					<div class="pl-grid">
						{#each redactedPlaylists as playlist (playlist.id)}
							<RedactedPlaylistCard {playlist} ondelete={handleCardDelete} />
						{/each}
					</div>
				</section>
			{/if}
		{/if}
	</div>
</div>

<style>
	.pl-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.875rem;
		margin-top: 0.5rem;
	}

	.pl-new {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem;
		max-width: 44rem;
	}

	/* one card across a phone, two on a desktop, three on a very wide screen */
	.pl-grid {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 1.25rem;
	}
	@media (min-width: 900px) {
		.pl-grid {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
	@media (min-width: 1800px) {
		.pl-grid {
			grid-template-columns: repeat(3, minmax(0, 1fr));
		}
	}

	/* the first playlist's place: a dashed tile that opens the name row */
	.pl-create {
		display: flex;
		align-items: center;
		gap: 1rem;
		min-height: 7.75rem;
		padding: 0.875rem;
		border: 2px dashed var(--dn-edge);
		border-radius: var(--radius-box);
		color: var(--color-base-content);
		text-align: left;
		transition:
			border-color 150ms,
			background-color 150ms;
	}
	.pl-create:hover {
		border-color: var(--dn-title);
		background-color: color-mix(in oklab, var(--dn-title) 6%, transparent);
	}
	.pl-create__plus {
		display: grid;
		flex: none;
		place-items: center;
		width: 6rem;
		aspect-ratio: 1;
		border: 1px solid var(--dn-line);
		border-radius: 0.375rem;
		color: var(--dn-title);
	}
	.pl-create__title {
		display: block;
		font-family: var(--font-display);
		font-size: 1.5rem;
		line-height: 1.05;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.pl-create__sub {
		display: block;
		margin-top: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	@media (max-width: 767px) {
		.pl-actions {
			display: grid;
			margin-top: 0;
		}
		.pl-actions :global(.btn) {
			width: 100%;
		}
	}
	@media (max-width: 479px) {
		.pl-create__plus {
			width: 5rem;
		}
		.pl-create__title {
			font-size: 1.3125rem;
		}
	}
</style>
