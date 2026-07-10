<script lang="ts">
	import { goto } from '$app/navigation';
	import { Loader2, Music2, ArrowLeft, RefreshCw, CheckCircle2, Download } from 'lucide-svelte';
	import PageHero from '$lib/ui/PageHero.svelte';
	import SpotifyIcon from '$lib/components/SpotifyIcon.svelte';
	import { toastStore } from '$lib/stores/toast';
	import {
		getSpotifyPlaylistsQuery,
		createImportSpotifyPlaylistMutation
	} from '$lib/queries/spotify/SpotifyQueries.svelte';
	import type { SpotifyPlaylistItem } from '$lib/types';

	const playlistsQuery = getSpotifyPlaylistsQuery();
	const importMutation = createImportSpotifyPlaylistMutation();

	let importing = $state<string | null>(null);
	let importingAll = $state(false);
	let importAllProgress = $state({ done: 0, total: 0 });

	async function handleImport(playlist: SpotifyPlaylistItem) {
		if (importing || importingAll) return;
		importing = playlist.id;
		try {
			const result = await importMutation.mutateAsync({ id: playlist.id, name: playlist.name });
			toastStore.show({
				message: `"${playlist.name}" is importing in the background`,
				type: 'success'
			});
			await goto(`/playlists/${result.playlist_id}`);
		} catch {
			toastStore.show({ message: `Failed to import "${playlist.name}"`, type: 'error' });
		} finally {
			importing = null;
		}
	}

	async function handleImportAll() {
		if (importing || importingAll) return;
		const unimported = (playlistsQuery.data?.playlists ?? []).filter(
			(p) => !p.imported_playlist_id
		);
		if (unimported.length === 0) return;

		importingAll = true;
		importAllProgress = { done: 0, total: unimported.length };
		let failed = 0;

		for (const playlist of unimported) {
			try {
				await importMutation.mutateAsync({ id: playlist.id, name: playlist.name });
			} catch {
				failed++;
			}
			importAllProgress = { done: importAllProgress.done + 1, total: unimported.length };
		}

		importingAll = false;
		if (failed === 0) {
			toastStore.show({
				message: `All ${unimported.length} playlists are importing in the background`,
				type: 'success'
			});
		} else {
			toastStore.show({
				message: `Started ${unimported.length - failed} of ${unimported.length} imports (${failed} failed)`,
				type: 'info'
			});
		}
	}

	const unimportedCount = $derived(
		(playlistsQuery.data?.playlists ?? []).filter((p) => !p.imported_playlist_id).length
	);

	const gridClass =
		'grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4';
</script>

<svelte:head>
	<title>Import from Spotify - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHero
		title="Import from Spotify"
		subtitle="Bring your Spotify playlists into the collection, matched track by track."
		eyebrow="Playlists"
		loading={playlistsQuery.isLoading}
	>
		{#snippet icon()}
			<SpotifyIcon class="h-7 w-7" />
		{/snippet}
		{#snippet actions()}
			<a
				href="/playlists"
				class="btn btn-ghost btn-sm gap-1.5 rounded-full bg-base-content/6"
				aria-label="Back to Playlists"
			>
				<ArrowLeft class="h-4 w-4" />
				Playlists
			</a>
			{#if unimportedCount > 0 && !importingAll}
				<button
					class="btn btn-primary btn-sm gap-1.5 rounded-full"
					onclick={() => void handleImportAll()}
					disabled={!!importing}
				>
					<Download class="h-3.5 w-3.5" />
					Import all ({unimportedCount})
				</button>
			{:else if importingAll}
				<div class="flex shrink-0 items-center gap-2 text-sm text-base-content/60">
					<Loader2 class="h-4 w-4 animate-spin" />
					{importAllProgress.done}/{importAllProgress.total}
				</div>
			{/if}
		{/snippet}
	</PageHero>

	<div class="space-y-6 px-4 pb-12 sm:px-6 lg:px-8">
		{#if playlistsQuery.isLoading}
			<div class={gridClass}>
				{#each Array(12) as _, i (`skel-${i}`)}
					<div class="aspect-square animate-pulse rounded-2xl bg-base-200"></div>
				{/each}
			</div>
		{:else if playlistsQuery.isError}
			{@const err = playlistsQuery.error}
			<div role="alert" class="alert alert-error">
				<span>
					{err instanceof Error && err.message.includes('400')
						? 'Spotify is not connected. Go to your profile to link your account.'
						: 'Failed to load Spotify playlists.'}
				</span>
				<div class="flex gap-2">
					{#if err instanceof Error && err.message.includes('400')}
						<a href="/profile" class="btn btn-sm btn-ghost">Go to Profile</a>
					{:else}
						<button class="btn btn-sm btn-ghost" onclick={() => void playlistsQuery.refetch()}>
							Retry
						</button>
					{/if}
				</div>
			</div>
		{:else if (playlistsQuery.data?.playlists ?? []).length === 0}
			<div
				class="flex flex-col items-center justify-center gap-4 rounded-2xl border border-dashed border-base-content/12 py-20"
			>
				<SpotifyIcon class="h-16 w-16 text-base-content/20" />
				<p class="text-base-content/50">No Spotify playlists found.</p>
			</div>
		{:else}
			<div class={gridClass}>
				{#each playlistsQuery.data?.playlists ?? [] as playlist (playlist.id)}
					{@const isImporting = importing === playlist.id}
					{@const alreadyImported = !!playlist.imported_playlist_id}
					<div
						class="group flex flex-col rounded-2xl border border-base-content/8 bg-base-200/50 p-3 transition-colors hover:border-primary/30"
					>
						<div class="relative aspect-square overflow-hidden rounded-xl bg-base-content/6">
							{#if playlist.cover_url}
								<img
									src={playlist.cover_url}
									alt={playlist.name}
									class="h-full w-full object-cover transition-transform duration-200 group-hover:scale-[1.03]"
								/>
							{:else}
								<div class="flex h-full w-full items-center justify-center">
									<Music2 class="h-10 w-10 text-base-content/20" />
								</div>
							{/if}

							{#if alreadyImported}
								<div
									class="absolute inset-0 flex flex-col items-center justify-center gap-2 bg-base-100/75 opacity-0 backdrop-blur-sm transition-opacity group-hover:opacity-100"
								>
									<a
										href="/playlists/{playlist.imported_playlist_id}"
										class="btn btn-sm btn-primary rounded-full"
									>
										View Playlist
									</a>
									<button
										class="btn btn-xs gap-1 rounded-full btn-ghost bg-base-content/10 hover:bg-base-content/20"
										onclick={() => void handleImport(playlist)}
										disabled={!!importing}
									>
										{#if isImporting}
											<Loader2 class="h-3 w-3 animate-spin" />
										{:else}
											<RefreshCw class="h-3 w-3" />
										{/if}
										Re-import
									</button>
								</div>
							{:else}
								<button
									class="absolute inset-0 flex items-center justify-center bg-base-100/75 opacity-0 backdrop-blur-sm transition-opacity group-hover:opacity-100 disabled:cursor-not-allowed"
									onclick={() => void handleImport(playlist)}
									disabled={!!importing}
								>
									{#if isImporting}
										<Loader2 class="h-8 w-8 animate-spin text-base-content" />
									{:else}
										<div
											class="flex flex-col items-center gap-1 rounded-xl px-3 py-2 text-base-content"
										>
											<SpotifyIcon class="h-6 w-6 text-accent" />
											<span class="font-mono text-xs font-bold uppercase tracking-wider">
												Import
											</span>
										</div>
									{/if}
								</button>
							{/if}

							{#if alreadyImported && !isImporting}
								<div
									class="absolute right-2 top-2 rounded-full bg-accent p-0.5 text-accent-content shadow transition-opacity group-hover:opacity-0"
								>
									<CheckCircle2 class="h-4 w-4" />
								</div>
							{/if}
						</div>

						<div class="mt-2.5 min-w-0 px-0.5">
							<p class="truncate font-display text-sm font-semibold leading-tight tracking-tight">
								{playlist.name}
							</p>
							{#if playlist.owner}
								<p class="truncate text-xs text-base-content/40">{playlist.owner}</p>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
