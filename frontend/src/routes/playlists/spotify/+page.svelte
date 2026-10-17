<script lang="ts">
	import { goto } from '$app/navigation';
	import { LoaderCircle, ArrowLeft, RefreshCw, CircleCheckBig, Download } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import SpotifyIcon from '$lib/components/SpotifyIcon.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { toastStore } from '$lib/stores/toast';
	import {
		getSpotifyPlaylistsQuery,
		createImportSpotifyPlaylistMutation
	} from '$lib/queries/spotify/SpotifyQueries.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	const playlistsQuery = getSpotifyPlaylistsQuery();
	const importMutation = createImportSpotifyPlaylistMutation();

	let importing = $state<string | null>(null);
	let importingAll = $state(false);
	let importAllProgress = $state({ done: 0, total: 0 });

	async function handleImport(playlist: { id: string; name: string }) {
		if (importing || importingAll) return;
		importing = playlist.id;
		try {
			const result = await importMutation.mutateAsync({ id: playlist.id, name: playlist.name });
			toastStore.show({
				message: `"${playlist.name}" is importing in the background`,
				type: 'success'
			});
			await goto(withBasePath(`/playlists/${result.playlist_id}`));
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

<div class="space-y-6 px-4 sm:px-6 lg:px-8">
	<div class="flex items-center gap-3">
		<IconButton
			size="sm"
			icon={ArrowLeft}
			label="Back to playlists"
			href={withBasePath('/playlists')}
		/>
		<h1 class="flex min-w-0 flex-1 items-center gap-2 text-2xl font-bold sm:text-3xl">
			<SpotifyIcon class="h-6 w-6 shrink-0 text-spotify" />
			Import from Spotify
		</h1>
		{#if unimportedCount > 0 && !importingAll}
			<Button
				size="sm"
				pill
				icon={Download}
				class="shrink-0"
				onclick={() => void handleImportAll()}
				disabled={!!importing}
			>
				Import all ({unimportedCount})
			</Button>
		{:else if importingAll}
			<div class="flex shrink-0 items-center gap-2 text-sm text-base-content/60">
				<LoaderCircle class="h-4 w-4 animate-spin motion-keep" />
				{importAllProgress.done}/{importAllProgress.total}
			</div>
		{/if}
	</div>

	{#if playlistsQuery.isLoading}
		<div class={gridClass}>
			{#each Array(12) as _, i (`skel-${i}`)}
				<div class="aspect-square animate-pulse rounded-2xl bg-base-300/60"></div>
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
					<Button variant="ghost" size="sm" href={withBasePath('/profile')}>Go to Profile</Button>
				{:else}
					<Button variant="ghost" size="sm" onclick={() => void playlistsQuery.refetch()}>
						Retry
					</Button>
				{/if}
			</div>
		</div>
	{:else if (playlistsQuery.data?.playlists ?? []).length === 0}
		<div class="flex flex-col items-center justify-center gap-4 py-20">
			<SpotifyIcon class="h-16 w-16 text-base-content/20" />
			<p class="text-base-content/50">No Spotify playlists found.</p>
		</div>
	{:else}
		<div class={gridClass}>
			{#each playlistsQuery.data?.playlists ?? [] as playlist (playlist.id)}
				{@const isImporting = importing === playlist.id}
				{@const alreadyImported = !!playlist.imported_playlist_id}
				<div class="group flex flex-col gap-2">
					<div class="relative aspect-square overflow-hidden rounded-2xl bg-base-300/60">
						<div
							class="h-full w-full transition-transform duration-200 motion-safe:group-hover:scale-[1.03]"
						>
							<AlbumImage
								customUrl={playlist.cover_url ?? null}
								alt={playlist.name}
								size="full"
								requestSize={250}
								rounded="none"
								className="h-full w-full"
							/>
						</div>

						{#if alreadyImported}
							<div
								class="absolute inset-0 flex flex-col items-center justify-center gap-2 bg-black/60 opacity-0 transition-opacity group-hover:opacity-100"
							>
								<Button
									size="sm"
									pill
									href={withBasePath(`/playlists/${playlist.imported_playlist_id}`)}
								>
									View Playlist
								</Button>
								<Button
									size="xs"
									pill
									icon={RefreshCw}
									loading={isImporting}
									onclick={() => void handleImport(playlist)}
									disabled={!!importing}
								>
									Re-import
								</Button>
							</div>
						{:else}
							<button
								type="button"
								class="absolute inset-0 flex items-center justify-center bg-black/60 opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100 disabled:cursor-not-allowed"
								aria-label="Import {playlist.name}"
								onclick={() => void handleImport(playlist)}
								disabled={!!importing}
							>
								{#if isImporting}
									<LoaderCircle class="h-8 w-8 animate-spin motion-keep text-white" />
								{:else}
									<div class="flex flex-col items-center gap-1 rounded-xl px-3 py-2 text-white">
										<SpotifyIcon class="h-6 w-6 text-spotify" />
										<span class="text-xs font-semibold">Import</span>
									</div>
								{/if}
							</button>
						{/if}

						{#if alreadyImported && !isImporting}
							<div
								class="absolute right-2 top-2 rounded-full bg-success p-0.5 shadow group-hover:opacity-0 transition-opacity"
							>
								<CircleCheckBig class="h-4 w-4 text-success-content" />
							</div>
						{/if}
					</div>

					<div class="min-w-0 px-0.5">
						<p class="truncate text-sm font-semibold leading-tight">{playlist.name}</p>
						{#if playlist.owner}
							<p class="truncate text-xs text-base-content/40">{playlist.owner}</p>
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>
