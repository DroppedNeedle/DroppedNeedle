<script lang="ts">
	import { goto } from '$app/navigation';
	import {
		LoaderCircle,
		ArrowLeft,
		RefreshCw,
		Check,
		CircleCheck,
		Download,
		TriangleAlert,
		UserRound
	} from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import MediaTileSkeleton from '$lib/media/MediaTileSkeleton.svelte';
	import SpotifyIcon from '$lib/media/icons/SpotifyIcon.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { toastStore } from '$lib/stores/toast';
	import {
		getSpotifyPlaylistsQuery,
		createImportSpotifyPlaylistMutation
	} from '$lib/features/playlists/queries/spotify.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { providerUnavailableMessage } from '$lib/utils/serviceError';

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
		} catch (error) {
			toastStore.show({
				message: providerUnavailableMessage(error) ?? `Failed to import "${playlist.name}"`,
				type: 'error'
			});
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

	const playlistList = $derived(playlistsQuery.data?.playlists ?? []);
	const importedCount = $derived(playlistList.length - unimportedCount);
	const notConnected = $derived(
		playlistsQuery.error instanceof Error && playlistsQuery.error.message.includes('400')
	);
</script>

<svelte:head>
	<title>Import from Spotify - DroppedNeedle</title>
</svelte:head>

<div class="space-y-10 px-4 pt-5 pb-10 sm:px-6 lg:px-8">
	<header class="sp-head">
		<div class="sp-head__title-row">
			<IconButton
				variant="outline"
				icon={ArrowLeft}
				label="Back to playlists"
				href={withBasePath('/playlists')}
			/>
			<div class="min-w-0">
				<h1 class="sp-head__title">Import from Spotify</h1>
				<p class="sp-head__line">Bring your own Spotify playlists into DroppedNeedle.</p>
			</div>
		</div>

		<div class="sp-head__body">
			<div class="dn-decor sp-sticker" aria-hidden="true">
				<Card variant="torn" padding="none" class="sp-sticker__paper">
					<span class="sp-sticker__logo">
						<SpotifyIcon class="size-full" />
					</span>
				</Card>
			</div>

			<div class="sp-head__status">
				{#if playlistsQuery.isLoading}
					<div class="sp-status" aria-busy="true">
						<div class="skeleton size-6 rounded-full"></div>
						<div class="flex flex-1 flex-col gap-2">
							<div class="skeleton h-4 w-40"></div>
							<div class="skeleton h-3 w-56"></div>
						</div>
					</div>
				{:else if playlistsQuery.isError}
					<div class="sp-status sp-status--{notConnected ? 'warning' : 'error'}" role="alert">
						<TriangleAlert class="sp-status__icon" aria-hidden="true" />
						<p class="min-w-0 flex-1">
							{notConnected
								? 'Spotify is not connected. Go to your profile to link your account.'
								: (providerUnavailableMessage(playlistsQuery.error) ??
									'Failed to load Spotify playlists.')}
						</p>
					</div>
					<div>
						{#if notConnected}
							<Button
								variant="primary"
								size="display"
								icon={UserRound}
								href={withBasePath('/profile')}
							>
								Go to Profile
							</Button>
						{:else}
							<Button
								variant="primary"
								size="display"
								icon={RefreshCw}
								onclick={() => void playlistsQuery.refetch()}
							>
								Retry
							</Button>
						{/if}
					</div>
				{:else}
					<div class="sp-status sp-status--ok">
						<CircleCheck class="sp-status__icon" aria-hidden="true" />
						<div class="min-w-0 flex-1">
							<p class="font-semibold">Spotify connected</p>
							<p class="sp-status__meta">
								{playlistList.length} playlist{playlistList.length === 1 ? '' : 's'} · {importedCount}
								imported
							</p>
						</div>
					</div>
					{#if importingAll}
						<p class="sp-progress" role="status">
							<LoaderCircle class="size-5 animate-spin motion-keep" aria-hidden="true" />
							Importing {importAllProgress.done}/{importAllProgress.total}
						</p>
					{:else if unimportedCount > 0}
						<div>
							<Button
								variant="primary"
								size="display"
								icon={Download}
								onclick={() => void handleImportAll()}
								disabled={!!importing}
							>
								Import all ({unimportedCount})
							</Button>
						</div>
					{/if}
				{/if}
			</div>
		</div>
	</header>

	{#if playlistsQuery.isLoading}
		<div class="dn-list-grid" aria-busy="true">
			{#each Array(12) as _, i (`skel-${i}`)}
				<MediaTileSkeleton />
			{/each}
		</div>
	{:else if playlistsQuery.isSuccess && playlistList.length === 0}
		<p class="py-12 text-center text-muted">No Spotify playlists found.</p>
	{:else if playlistList.length > 0}
		<section aria-label="Spotify playlists">
			<div class="dn-list-grid">
				{#each playlistList as playlist (playlist.id)}
					{@const isImporting = importing === playlist.id}
					{@const alreadyImported = !!playlist.imported_playlist_id}
					<MediaTile title={playlist.name}>
						{#snippet art()}
							<AlbumImage
								customUrl={playlist.cover_url ?? null}
								alt={playlist.name}
								size="full"
								requestSize={250}
								rounded="lg"
								className="h-full w-full"
							/>
						{/snippet}
						{#snippet meta()}
							{playlist.owner}
						{/snippet}
						{#snippet overlay()}
							{#if alreadyImported}
								{#if !isImporting}
									<span class="sp-badge" role="img" aria-label="Imported">
										<Check class="size-4" aria-hidden="true" />
									</span>
								{/if}
								<div class="sp-cover-actions">
									<Button
										variant="primary"
										size="display-sm"
										href={withBasePath(`/playlists/${playlist.imported_playlist_id}`)}
									>
										View Playlist
									</Button>
									<Button
										variant="outline"
										size="sm"
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
									class="sp-cover-actions sp-cover-actions--import"
									class:is-busy={isImporting}
									aria-label="Import {playlist.name}"
									onclick={() => void handleImport(playlist)}
									disabled={!!importing}
								>
									{#if isImporting}
										<LoaderCircle class="size-8 animate-spin motion-keep" aria-hidden="true" />
									{:else}
										<SpotifyIcon class="size-8 text-spotify" />
										<span class="sp-import-label">Import</span>
									{/if}
								</button>
							{/if}
						{/snippet}
					</MediaTile>
				{/each}
			</div>
		</section>
	{/if}
</div>

<style>
	.sp-head__title-row {
		display: flex;
		align-items: flex-start;
		gap: 1rem;
	}
	.sp-head__title-row :global(.btn) {
		margin-top: 0.75rem;
		flex: none;
	}
	.sp-head__title {
		font-size: clamp(2.5rem, 1rem + 5vw, 6rem);
		line-height: 0.95;
		overflow-wrap: anywhere;
	}
	.sp-head__line {
		margin-top: 0.5rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}

	.sp-head__body {
		display: flex;
		align-items: center;
		gap: 2.5rem;
		margin-top: 2rem;
	}

	/* the Spotify mark printed on a torn scrap of paper, slightly askew */
	.sp-sticker {
		flex: none;
		width: 10rem;
		rotate: -5deg;
		filter: drop-shadow(0 10px 18px rgb(0 0 0 / 0.45));
	}
	.sp-sticker :global(.sp-sticker__paper) {
		display: grid;
		aspect-ratio: 1;
		place-items: center;
	}
	.sp-sticker__logo {
		position: relative;
		display: block;
		width: 62%;
		aspect-ratio: 1;
		color: var(--dn-paper-content);
	}
	/* the mark's arcs show the green through them */
	.sp-sticker__logo::before {
		content: '';
		position: absolute;
		inset: 12%;
		z-index: -1;
		border-radius: 9999px;
		background-color: var(--dn-spotify);
	}

	.sp-head__status {
		display: flex;
		min-width: 0;
		max-width: 36rem;
		flex: 1;
		flex-direction: column;
		align-items: flex-start;
		gap: 1rem;
	}
	.sp-status {
		display: flex;
		width: 100%;
		align-items: center;
		gap: 0.875rem;
		padding: 0.875rem 1rem;
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-box);
		background-color: var(--color-base-200);
	}
	.sp-status :global(.sp-status__icon) {
		width: 1.5rem;
		height: 1.5rem;
		flex: none;
	}
	.sp-status--ok {
		border-color: color-mix(in oklab, var(--color-success) 55%, var(--dn-line));
	}
	.sp-status--ok :global(.sp-status__icon) {
		color: var(--color-success);
	}
	.sp-status--warning {
		border-color: color-mix(in oklab, var(--color-warning) 60%, transparent);
		background-color: color-mix(in oklab, var(--color-warning) 8%, transparent);
	}
	.sp-status--warning :global(.sp-status__icon) {
		color: var(--color-warning);
	}
	.sp-status--error {
		border-color: color-mix(in oklab, var(--color-error) 60%, transparent);
		background-color: color-mix(in oklab, var(--color-error) 8%, transparent);
	}
	.sp-status--error :global(.sp-status__icon) {
		color: var(--color-error);
	}
	.sp-status__meta {
		margin-top: 0.125rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.sp-progress {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
	}

	.sp-badge {
		position: absolute;
		top: 0.5rem;
		right: 0.5rem;
		display: grid;
		place-items: center;
		width: 1.75rem;
		height: 1.75rem;
		border-radius: 9999px;
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: 0 2px 6px rgb(0 0 0 / 0.4);
		transition: opacity 150ms;
	}

	/* the cover's own buttons, shown on hover or focus */
	.sp-cover-actions {
		position: absolute;
		inset: 0;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		border-radius: 0.5rem;
		background-color: rgb(0 0 0 / 0.62);
		color: #fff;
		opacity: 0;
		transition: opacity 150ms;
	}
	:global(.group:hover) .sp-cover-actions,
	.sp-cover-actions:focus-within,
	.sp-cover-actions--import:focus-visible,
	.sp-cover-actions.is-busy {
		opacity: 1;
	}
	:global(.group:hover) .sp-badge,
	:global(.group:focus-within) .sp-badge {
		opacity: 0;
	}
	.sp-cover-actions--import {
		cursor: pointer;
	}
	.sp-cover-actions--import:disabled {
		cursor: not-allowed;
	}
	.sp-import-label {
		font-family: var(--font-display);
		font-size: 1.125rem;
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}

	@media (hover: none) {
		.sp-cover-actions--import {
			inset: auto 0.5rem 0.5rem auto;
			width: 2.75rem;
			height: 2.75rem;
			border-radius: 9999px;
			opacity: 1;
		}
		.sp-cover-actions--import .sp-import-label {
			display: none;
		}
		.sp-cover-actions--import :global(svg) {
			width: 1.5rem;
			height: 1.5rem;
		}
	}

	@media (max-width: 639px) {
		.sp-head__body {
			gap: 1.25rem;
			margin-top: 1.25rem;
		}
		.sp-sticker {
			width: 6rem;
		}
		.sp-head__line {
			font-size: 0.75rem;
			letter-spacing: 0.1em;
		}
	}
	@media (max-width: 379px) {
		.sp-sticker {
			display: none;
		}
	}
</style>
