<script lang="ts">
	import { CircleAlert, CircleHelp } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		confirmAlbumEdition,
		getWaitingAlbumsQuery
	} from '$lib/queries/albums/EditionQueries.svelte';

	const PAGE = 50;
	let view = $state<'unconfirmed' | 'unmatched'>('unconfirmed');
	let offset = $state(0);

	const query = getWaitingAlbumsQuery(
		() => authStore.user?.id,
		() => view,
		() => offset,
		PAGE
	);
	const confirm = confirmAlbumEdition();
	const items = $derived(query.data?.items ?? []);
	const total = $derived(query.data?.total ?? 0);

	function show(next: 'unconfirmed' | 'unmatched'): void {
		view = next;
		offset = 0;
	}

	async function looksRight(albumId: string): Promise<void> {
		try {
			await confirm.mutateAsync({ userId: authStore.user?.id, localId: albumId });
			toastStore.show({ message: 'Confirmed.', type: 'success' });
		} catch (error) {
			const message =
				error instanceof ApiError || error instanceof Error
					? error.message
					: 'Could not confirm the match.';
			toastStore.show({ message, type: 'error' });
		}
	}
</script>

<div class="space-y-4">
	<p class="max-w-3xl text-sm text-base-content/65">
		When DroppedNeedle isn't sure which edition an album is, it still uses its best guess for the
		album page, playback, other apps and downloads, and lists the album here. Nothing waits on you:
		look through them when it suits you. Until you confirm one, tagging and organizing leave its
		files alone.
	</p>

	<div role="tablist" class="tabs tabs-boxed w-fit">
		<button
			role="tab"
			class="tab {view === 'unconfirmed' ? 'tab-active' : ''}"
			aria-selected={view === 'unconfirmed'}
			onclick={() => show('unconfirmed')}>Best guesses</button
		>
		<button
			role="tab"
			class="tab {view === 'unmatched' ? 'tab-active' : ''}"
			aria-selected={view === 'unmatched'}
			onclick={() => show('unmatched')}>No match</button
		>
	</div>

	{#if query.isLoading}
		<div class="space-y-2">
			{#each Array(6) as _, index (index)}<div class="skeleton h-16 w-full"></div>{/each}
		</div>
	{:else if query.isError}
		<div class="alert alert-error">
			<span>The list couldn't be loaded.</span>
			<button class="btn btn-sm" onclick={() => query.refetch()}>Retry</button>
		</div>
	{:else if !items.length}
		<div class="rounded-box bg-base-200 p-8 text-center text-base-content/60">
			{view === 'unconfirmed'
				? 'Every matched album is confirmed. Nothing to look at.'
				: 'Every album matches something on MusicBrainz.'}
		</div>
	{:else}
		<ul class="space-y-2">
			{#each items as item (item.album_id)}
				<li
					class="flex flex-wrap items-center justify-between gap-3 rounded-box border border-base-content/10 bg-base-100 p-3"
				>
					<div class="flex min-w-0 items-start gap-3">
						{#if item.state === 'unmatched'}
							<CircleAlert class="mt-0.5 h-4 w-4 shrink-0 text-error" />
						{:else}
							<CircleHelp class="mt-0.5 h-4 w-4 shrink-0 text-warning" />
						{/if}
						<div class="min-w-0">
							<a
								class="block truncate font-medium hover:underline"
								href={withBasePath(`/album/${encodeURIComponent(item.album_id)}`)}>{item.title}</a
							>
							<p class="truncate text-xs text-base-content/55">{item.artist_name}</p>
							<p class="mt-1 text-sm text-base-content/70">{item.reason.message}</p>
						</div>
					</div>
					<div class="flex gap-2">
						{#if authStore.isTrusted && item.state === 'unconfirmed'}
							<button
								class="btn btn-primary btn-sm"
								disabled={confirm.isPending}
								onclick={() => void looksRight(item.album_id)}>Looks right</button
							>
						{/if}
						<a
							class="btn btn-ghost btn-sm"
							href={withBasePath(`/album/${encodeURIComponent(item.album_id)}`)}
							>{item.state === 'unmatched' ? 'Pick an edition' : 'Compare editions'}</a
						>
					</div>
				</li>
			{/each}
		</ul>
		{#if total > PAGE}
			<div class="flex items-center justify-between text-sm">
				<span class="text-base-content/60"
					>{offset + 1}–{Math.min(offset + PAGE, total)} of {total}</span
				>
				<div class="join">
					<button
						class="btn btn-sm join-item"
						disabled={offset === 0}
						onclick={() => (offset = Math.max(0, offset - PAGE))}>Previous</button
					>
					<button
						class="btn btn-sm join-item"
						disabled={offset + PAGE >= total}
						onclick={() => (offset += PAGE)}>Next</button
					>
				</div>
			</div>
		{/if}
	{/if}
</div>
