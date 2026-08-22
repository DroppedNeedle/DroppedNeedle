<script lang="ts">
	import { resolve } from '$app/paths';
	import { ApiError } from '$lib/api/client';
	import BackButton from '$lib/components/BackButton.svelte';
	import { createSourcePlaylistImportMutation } from '$lib/queries/source-playlists/SourcePlaylistMutations.svelte';
	import { getSourcePlaylistDetailQuery } from '$lib/queries/source-playlists/SourcePlaylistQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { formatTotalDurationSec } from '$lib/utils/formatting';
	import { Disc3, Download } from 'lucide-svelte';
	import type { SourcePlaylistSource } from '$lib/types';
	import type { Snippet } from 'svelte';

	interface Props {
		playlistId: string;
		source: SourcePlaylistSource;
		backFallback: string;
		icon: Snippet;
	}

	let { playlistId, source, backFallback, icon }: Props = $props();
	const detailQuery = getSourcePlaylistDetailQuery(
		() => source,
		() => playlistId
	);
	const importMutation = createSourcePlaylistImportMutation(() => source);
	const detail = $derived(detailQuery.data);
	const importResult = $derived(importMutation.data ?? null);
	const relinkRequired = $derived(
		detailQuery.error instanceof ApiError &&
			detailQuery.error.code === 'MEDIA_ACCOUNT_RELINK_REQUIRED'
	);
	const notFound = $derived(
		detailQuery.error instanceof ApiError && detailQuery.error.status === 404
	);
	const importRelinkRequired = $derived(
		importMutation.error instanceof ApiError &&
			importMutation.error.code === 'MEDIA_ACCOUNT_RELINK_REQUIRED'
	);

	async function handleImport() {
		if (!playlistId || importMutation.isPending) return;
		try {
			const result = await importMutation.mutateAsync(playlistId);
			if (result.already_imported) {
				toastStore.show({ message: 'This playlist is already in DroppedNeedle.', type: 'info' });
			} else {
				toastStore.show({
					message: `Imported ${result.tracks_imported} tracks into DroppedNeedle.`,
					type: 'success'
				});
			}
		} catch (error) {
			const message =
				error instanceof ApiError && error.code === 'MEDIA_ACCOUNT_RELINK_REQUIRED'
					? 'Reconnect this account before importing the playlist.'
					: "Couldn't import this playlist.";
			toastStore.show({ message, type: 'error' });
		}
	}
</script>

<div class="max-w-4xl mx-auto px-4 py-6 space-y-6">
	<div class="flex items-center gap-3">
		<BackButton fallback={backFallback} />
		{@render icon()}
	</div>

	{#if detailQuery.isPending}
		<div class="flex flex-col gap-6 sm:flex-row">
			<div class="skeleton h-48 w-48 shrink-0 rounded-lg"></div>
			<div class="flex-1 space-y-3 py-2">
				<div class="skeleton h-8 w-2/3"></div>
				<div class="skeleton h-4 w-40"></div>
				<div class="skeleton h-8 w-44"></div>
			</div>
		</div>
	{:else if detailQuery.isError}
		<div class="alert alert-warning alert-soft">
			<span>
				{relinkRequired
					? 'Reconnect this account before opening the playlist.'
					: notFound
						? 'This playlist is no longer available to the connected account.'
						: "Couldn't load this playlist."}
			</span>
			{#if relinkRequired}
				<a class="btn btn-primary btn-sm" href={resolve('/profile#media-accounts')}>Reconnect</a>
			{:else if notFound}
				<a class="btn btn-primary btn-sm" href={backFallback}>Back to playlists</a>
			{:else}
				<button class="btn btn-ghost btn-sm" onclick={() => void detailQuery.refetch()}
					>Retry</button
				>
			{/if}
		</div>
	{:else if detail}
		<div class="flex flex-col sm:flex-row gap-6">
			<div class="w-48 h-48 shrink-0 rounded-lg overflow-hidden shadow-md">
				{#if detail.cover_url}
					<img src={detail.cover_url} alt={detail.name} class="w-full h-full object-cover" />
				{:else}
					<div class="w-full h-full bg-base-200 flex items-center justify-center">
						<Disc3 class="w-16 h-16 text-base-content/20" />
					</div>
				{/if}
			</div>
			<div class="space-y-2">
				<h1 class="hero-title text-2xl font-bold">{detail.name}</h1>
				<p class="text-base-content/60">
					{detail.track_count} track{detail.track_count === 1 ? '' : 's'}
					{#if detail.duration_seconds}
						- {formatTotalDurationSec(detail.duration_seconds)}
					{/if}
				</p>
				<button
					class="btn btn-primary btn-sm gap-2"
					onclick={handleImport}
					disabled={importMutation.isPending || importResult?.already_imported}
				>
					{#if importMutation.isPending}
						<span class="loading loading-spinner loading-xs"></span>
					{:else}
						<Download class="w-4 h-4" />
					{/if}
					{importResult?.already_imported
						? 'Already in DroppedNeedle'
						: 'Import into DroppedNeedle'}
				</button>
				{#if importResult && !importResult.already_imported}
					<p class="text-sm text-success">
						Imported {importResult.tracks_imported} tracks
						{#if importResult.tracks_failed > 0}
							({importResult.tracks_failed} skipped)
						{/if}
					</p>
				{/if}
				{#if importRelinkRequired}
					<p class="text-sm text-error">
						Reconnect the linked account to continue.
						<a class="link font-medium" href={resolve('/profile#media-accounts')}>Reconnect</a>
					</p>
				{/if}
			</div>
		</div>

		{#if detail.tracks.length > 0}
			<div class="overflow-x-auto">
				<table class="table table-sm">
					<thead>
						<tr>
							<th class="w-12">#</th>
							<th>Title</th>
							<th>Artist</th>
							<th>Album</th>
							<th class="text-right">Duration</th>
						</tr>
					</thead>
					<tbody>
						{#each detail.tracks as track, i (track.id)}
							<tr class="hover:bg-base-200/50">
								<td class="text-base-content/40">{i + 1}</td>
								<td class="font-medium">{track.track_name}</td>
								<td class="text-base-content/70">{track.artist_name}</td>
								<td class="text-base-content/70">{track.album_name}</td>
								<td class="text-right text-base-content/50">
									{#if track.duration_seconds}
										{Math.floor(track.duration_seconds / 60)}:{String(
											track.duration_seconds % 60
										).padStart(2, '0')}
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	{/if}
</div>
