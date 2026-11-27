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
	import { FileText, Plus, RefreshCw, Search, TriangleAlert, Upload } from 'lucide-svelte';
	import SpotifyIcon from '$lib/media/icons/SpotifyIcon.svelte';
	import MadeForYouShelf from '$lib/features/playlists/components/MadeForYouShelf.svelte';
	import PlaylistImport from '$lib/features/playlists/components/PlaylistImport.svelte';
	import PlaylistRow from '$lib/features/playlists/components/PlaylistRow.svelte';
	import PlaylistRowSkeleton from '$lib/features/playlists/components/PlaylistRowSkeleton.svelte';
	import RedactedPlaylistRow from '$lib/features/playlists/components/RedactedPlaylistRow.svelte';
	import { isMadeForYou } from '$lib/features/playlists/components/madeForYou';
	import Button from '$lib/ui/Button.svelte';
	import ContextMenu, { type MenuItem } from '$lib/ui/ContextMenu.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import SegmentedChoice from '$lib/ui/SegmentedChoice.svelte';

	type Sort = 'recent' | 'name';

	const connectionsQuery = getConnectionsQuery();
	const spotifyLinked = $derived(
		connectionsQuery.data?.connections.some((c) => c.service === 'spotify') ?? false
	);

	const query = getPlaylistListV3Query(() => authStore.isAuthenticated);
	const createMutation = createPlaylistV3Mutation();

	let items = $derived(toPageList(query.data ?? []));
	let summaries = $derived(items.filter((p): p is PlaylistSummary => !isRedactedPlaylist(p)));
	let madeForYou = $derived(summaries.filter(isMadeForYou));
	let myPlaylists = $derived(summaries.filter((p) => p.is_owner && !isMadeForYou(p)));
	let sharedPlaylists = $derived(summaries.filter((p) => !p.is_owner));
	let redactedPlaylists = $derived(items.filter(isRedactedPlaylist) as RedactedPlaylist[]);
	let errorMessage = $derived(
		query.isError
			? query.error instanceof Error
				? query.error.message
				: "Couldn't load playlists"
			: null
	);

	let search = $state('');
	let searchOpen = $state(false);
	let sort = $state<Sort>('recent');
	const SORTS: { value: Sort; label: string }[] = [
		{ value: 'recent', label: 'Recent' },
		{ value: 'name', label: 'A-Z' }
	];

	let shownPlaylists = $derived.by(() => {
		const needle = search.trim().toLocaleLowerCase();
		const found = needle
			? myPlaylists.filter((p) => p.name.toLocaleLowerCase().includes(needle))
			: [...myPlaylists];
		return sort === 'name'
			? found.sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }))
			: found.sort((a, b) => b.updated_at.localeCompare(a.updated_at));
	});

	let importOpen = $state(false);
	let importItems = $derived<MenuItem[]>([
		{ label: 'From a file', icon: FileText, onclick: () => (importOpen = true) },
		...(spotifyLinked
			? [
					{
						label: 'From Spotify',
						icon: SpotifyIcon,
						onclick: () => void goto(withBasePath('/playlists/spotify'))
					}
				]
			: [])
	]);

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

	function handleDelete() {
		void query.refetch();
	}
</script>

<svelte:head>
	<title>Playlists - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<ListPageHeader title="Playlists" ticket="DN / CC 3.2">
		{#snippet actions()}
			<div class="pl-actions">
				<Button variant="primary" size="display" icon={Plus} onclick={() => (showNewInput = true)}>
					New playlist
				</Button>
				<ContextMenu items={importItems} trigger="text" label="Import" textIcon={Upload} />
			</div>
		{/snippet}
	</ListPageHeader>
	<PlaylistImport bind:open={importOpen} />

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
			<div class="pl-rows" aria-busy="true">
				{#each Array(8) as _, i (`skeleton-${i}`)}
					<PlaylistRowSkeleton />
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
			<MadeForYouShelf lists={madeForYou} />

			<section aria-label="Your playlists">
				<SectionHead title="Your playlists" wrap>
					{#snippet chips()}
						<span class="pl-count">{myPlaylists.length}</span>
					{/snippet}
					{#snippet actions()}
						{#if myPlaylists.length > 0}
							<div class="pl-tools">
								<IconButton
									variant={searchOpen ? 'neutral' : 'ghost'}
									size="sm"
									icon={Search}
									label="Find a playlist"
									class="sm:hidden"
									aria-expanded={searchOpen}
									onclick={() => (searchOpen = !searchOpen)}
								/>
								<div class="pl-search hidden sm:block">
									<Input
										type="search"
										size="sm"
										label="Find a playlist"
										placeholder="Find a playlist"
										icon={Search}
										bind:value={search}
									/>
								</div>
								<SegmentedChoice label="Sort" look="caps" options={SORTS} bind:value={sort} />
							</div>
						{/if}
					{/snippet}
				</SectionHead>
				{#if searchOpen}
					<div class="mb-3 sm:hidden">
						<Input
							type="search"
							size="sm"
							label="Find a playlist"
							placeholder="Find a playlist"
							icon={Search}
							class="w-full"
							bind:value={search}
						/>
					</div>
				{/if}
				{#if myPlaylists.length === 0}
					<button type="button" class="pl-create" onclick={() => (showNewInput = true)}>
						<span class="pl-create__plus" aria-hidden="true"><Plus class="size-5" /></span>
						<span class="min-w-0">
							<span class="pl-create__title">Create your first playlist</span>
							<span class="pl-create__sub">No playlists yet</span>
						</span>
					</button>
				{:else}
					<div class="pl-rows">
						{#each shownPlaylists as playlist (playlist.id)}
							<PlaylistRow {playlist} />
						{:else}
							<p class="pl-none">No playlist matches "{search.trim()}"</p>
						{/each}
					</div>
				{/if}
			</section>

			{#if sharedPlaylists.length > 0 || redactedPlaylists.length > 0}
				<section aria-label="Shared with you">
					<SectionHead title="Shared with you">
						{#snippet chips()}
							<span class="pl-count">{sharedPlaylists.length + redactedPlaylists.length}</span>
						{/snippet}
					</SectionHead>
					<div class="pl-rows">
						{#each sharedPlaylists as playlist (playlist.id)}
							<PlaylistRow {playlist} />
						{/each}
						{#each redactedPlaylists as playlist (playlist.id)}
							<RedactedPlaylistRow {playlist} ondelete={handleDelete} />
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
		gap: 0.625rem;
	}

	.pl-new {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem;
		max-width: 44rem;
	}

	.pl-count {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		color: var(--dn-muted);
	}

	.pl-tools {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}
	.pl-search {
		width: 16rem;
	}

	/* small rows: one across a phone, up to four on a wide screen */
	.pl-rows {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 0.375rem 0.75rem;
	}
	@media (min-width: 768px) {
		.pl-rows {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
	@media (min-width: 1100px) {
		.pl-rows {
			grid-template-columns: repeat(3, minmax(0, 1fr));
		}
	}
	@media (min-width: 1400px) {
		.pl-rows {
			grid-template-columns: repeat(4, minmax(0, 1fr));
		}
	}

	.pl-none {
		grid-column: 1 / -1;
		padding: 0.75rem 0.375rem;
		font-size: 0.875rem;
		color: var(--dn-muted);
	}

	/* the first playlist's place: a dashed row that opens the name field */
	.pl-create {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		width: 100%;
		max-width: 24rem;
		padding: 0.375rem;
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
		width: 3.25rem;
		aspect-ratio: 1;
		border: 1px solid var(--dn-line);
		border-radius: 0.375rem;
		color: var(--dn-title);
	}
	.pl-create__title {
		display: block;
		font-size: 0.9375rem;
		font-weight: 600;
	}
	.pl-create__sub {
		display: block;
		margin-top: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
</style>
