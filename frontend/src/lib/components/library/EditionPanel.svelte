<script lang="ts">
	import { CircleAlert, CircleCheck, CircleHelp, Hand, Undo2 } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import EditionPicker from './EditionPicker.svelte';
	import {
		confirmAlbumEdition,
		getAlbumEditionStatusQuery,
		getAlbumEditionsQuery,
		undoAlbumEdition
	} from '$lib/queries/albums/EditionQueries.svelte';

	interface Props {
		/** Library album id. */
		albumId: string;
		albumTitle: string;
		/** The release group the album is matched to, when it is. */
		groupMbid: string;
		onchanged?: () => void;
	}

	let { albumId, albumTitle, groupMbid, onchanged }: Props = $props();

	const userId = () => authStore.user?.id;
	const statusQuery = getAlbumEditionStatusQuery(userId, () => albumId);
	const editionsQuery = getAlbumEditionsQuery(
		userId,
		() => groupMbid,
		() => Boolean(groupMbid)
	);
	const confirm = confirmAlbumEdition();
	const undo = undoAlbumEdition();
	let picker = $state<{ open: () => void } | null>(null);

	const status = $derived(statusQuery.data ?? null);
	const edition = $derived(
		(editionsQuery.data?.items ?? []).find((item) => item.release_mbid === status?.release_mbid) ??
			null
	);
	const heading = $derived(
		{
			chosen: 'Chosen edition',
			confirmed: 'Matched edition',
			unconfirmed: 'Best guess, not confirmed',
			unmatched: 'No match on MusicBrainz',
			unidentified: 'Not matched yet'
		}[status?.state ?? 'unidentified'] ?? 'Edition'
	);
	const tone = $derived(
		status?.state === 'unconfirmed'
			? 'border-warning/40 bg-warning/5'
			: status?.state === 'unmatched'
				? 'border-error/30 bg-error/5'
				: 'border-base-content/10 bg-base-200/35'
	);

	function failure(error: unknown, fallback: string): string {
		if (error instanceof ApiError) {
			const action = (error.details as { action?: unknown } | null)?.action;
			return typeof action === 'string' ? `${error.message} ${action}` : error.message;
		}
		return error instanceof Error ? error.message : fallback;
	}

	async function looksRight(): Promise<void> {
		try {
			await confirm.mutateAsync({ userId: userId(), localId: albumId, rgMbid: groupMbid });
			toastStore.show({ message: 'Confirmed. This edition is now yours.', type: 'success' });
			onchanged?.();
		} catch (error) {
			toastStore.show({ message: failure(error, 'Could not confirm the match.'), type: 'error' });
		}
	}

	async function takeBack(): Promise<void> {
		try {
			await undo.mutateAsync({ userId: userId(), localId: albumId, rgMbid: groupMbid });
			toastStore.show({ message: 'Your last edition change was taken back.', type: 'success' });
			onchanged?.();
		} catch (error) {
			toastStore.show({ message: failure(error, 'Could not undo the change.'), type: 'error' });
		}
	}
</script>

<section class="rounded-box border p-4 {tone}" aria-labelledby="edition-panel-title">
	{#if statusQuery.isLoading}
		<div class="skeleton h-16 w-full"></div>
	{:else if statusQuery.isError || !status}
		<p class="text-sm text-base-content/55">The album's edition couldn't be loaded.</p>
	{:else}
		<div class="flex flex-wrap items-start justify-between gap-3">
			<div class="min-w-0">
				<h2 id="edition-panel-title" class="flex items-center gap-2 font-semibold">
					{#if status.state === 'unconfirmed'}
						<CircleHelp class="h-4 w-4 text-warning" />
					{:else if status.state === 'unmatched'}
						<CircleAlert class="h-4 w-4 text-error" />
					{:else if status.state === 'chosen'}
						<Hand class="h-4 w-4 text-primary" />
					{:else}
						<CircleCheck class="h-4 w-4 text-success" />
					{/if}
					{heading}
				</h2>
				{#if edition}
					<p class="mt-1 text-sm">
						{edition.title ?? albumTitle}
						<span class="text-base-content/55">
							· {[
								edition.disambiguation,
								edition.date?.slice(0, 4),
								edition.country,
								(edition.media_formats ?? []).join(' + '),
								`${edition.track_count} tracks`
							]
								.filter(Boolean)
								.join(' · ')}
						</span>
					</p>
				{/if}
				<p class="mt-1 text-sm text-base-content/70">{status.reason.message}</p>
				<p class="text-xs text-base-content/50">{status.reason.action}</p>
			</div>
			{#if authStore.isTrusted}
				<div class="flex flex-wrap gap-2">
					{#if status.state === 'unconfirmed'}
						<button
							class="btn btn-primary btn-sm"
							disabled={confirm.isPending}
							onclick={() => void looksRight()}>Looks right</button
						>
					{/if}
					<button class="btn btn-outline btn-sm" onclick={() => picker?.open()}>
						{status.state === 'unconfirmed' || status.state === 'unmatched'
							? 'Pick another edition'
							: 'Change edition'}
					</button>
					{#if status.undo_available}
						<button
							class="btn btn-ghost btn-sm gap-1"
							disabled={undo.isPending}
							onclick={() => void takeBack()}><Undo2 class="h-4 w-4" /> Undo</button
						>
					{/if}
				</div>
			{/if}
		</div>
		{#if status.candidates.length && status.state !== 'chosen'}
			<details class="mt-3 text-sm">
				<summary class="cursor-pointer text-base-content/70">
					Why this was picked: the closest candidates
				</summary>
				<ul class="mt-2 space-y-1">
					{#each status.candidates as candidate, index (candidate.release_mbid ?? candidate.release_group_mbid + index)}
						<li
							class="flex flex-wrap items-baseline justify-between gap-2 rounded-lg bg-base-100 px-3 py-2"
						>
							<span class="min-w-0">
								<span class="font-medium">{candidate.album_title}</span>
								<span class="text-base-content/55"> · {candidate.album_artist_name}</span>
								{#if candidate.release_mbid === status.release_mbid}
									<span class="badge badge-primary badge-xs ml-1">picked</span>
								{/if}
							</span>
							<span class="text-xs text-base-content/55">
								{candidate.matched_files} files fit · {Math.round(candidate.score * 100)}% match
								{#if candidate.penalties.length}
									· mostly off on {candidate.penalties[0].name.replaceAll('_', ' ')}
								{/if}
							</span>
						</li>
					{/each}
				</ul>
			</details>
		{/if}
	{/if}
</section>

<EditionPicker
	bind:this={picker}
	{groupMbid}
	copies={[{ id: albumId, title: albumTitle }]}
	{onchanged}
/>
