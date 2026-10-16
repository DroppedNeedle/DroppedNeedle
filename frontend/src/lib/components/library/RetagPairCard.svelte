<!--
	Two albums the scan would not join although they look like one album
	retagged outside DroppedNeedle. An admin merges them or keeps them apart.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { Combine } from 'lucide-svelte';

	import { ApiError } from '$lib/api/client';
	import { getAlbumEditionsQuery } from '$lib/queries/albums/EditionQueries.svelte';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
	import {
		dismissRetagPair,
		joinRetagPair,
		type RetagPair
	} from '$lib/queries/library/LibraryRetagPairQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		pair: RetagPair;
		/** The album whose page shows the card. A merge that keeps the other one goes there. */
		currentAlbumId?: string;
	}

	let { pair, currentAlbumId }: Props = $props();

	const join = joinRetagPair();
	const dismiss = dismissRetagPair();

	let keep = $state<string | null>(null);
	let refusal = $state<string | null>(null);

	const oldAlbum = $derived(pair.old_album);
	const newAlbum = $derived(pair.new_album);
	const radioName = $derived(`retag-pair-keep-${oldAlbum.id}-${newAlbum.id}`);

	const oldEditions = getAlbumEditionsQuery(
		() => authStore.user?.id,
		() => oldAlbum.edition?.release_group_mbid ?? '',
		() => pair.editions_differ
	);
	const newEditions = getAlbumEditionsQuery(
		() => authStore.user?.id,
		() => newAlbum.edition?.release_group_mbid ?? '',
		() => pair.editions_differ
	);

	function tracks(count: number): string {
		return `${count} ${count === 1 ? 'track' : 'tracks'}`;
	}

	// "the 2011 GB CD edition", or null while the edition list is not there.
	function editionName(side: RetagPair['old_album'], fromOld: boolean): string | null {
		const release = side.edition?.release_mbid;
		const items = (fromOld ? oldEditions : newEditions).data?.items ?? [];
		const found = release ? items.find((item) => item.release_mbid === release) : undefined;
		if (!found) return null;
		const parts = [
			found.date?.slice(0, 4),
			found.country,
			(found.media_formats ?? []).join(' + '),
			found.disambiguation ? `(${found.disambiguation})` : null
		].filter(Boolean);
		return parts.length > 0 ? `the ${parts.join(' ')} edition` : null;
	}

	function keepLabel(side: RetagPair['old_album'], fromOld: boolean): string {
		const name = editionName(side, fromOld);
		return name ? `Keep ${name} (from ${side.title})` : `Keep the edition chosen for ${side.title}`;
	}

	// Whose chosen edition the joined album ends up with, as far as the card can tell.
	const keepsOldEdition = $derived(
		pair.editions_differ
			? keep === oldAlbum.id
			: !!oldAlbum.edition?.chosen &&
					!!oldAlbum.edition.release_mbid &&
					!oldAlbum.edition.custom &&
					!newAlbum.edition?.chosen
	);
	const keptEditionName = $derived(
		editionName(oldAlbum, true) ?? `the edition chosen for ${oldAlbum.title}`
	);
	const mergeBlocked = $derived(pair.editions_differ && keep === null);

	async function settleRefusal(error: unknown): Promise<void> {
		if (!(error instanceof ApiError) || error.status < 400 || error.status >= 500) return;
		refusal = error.message;
		if (error.status === 409) {
			await invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.retagPairs() });
		}
	}

	async function merge(): Promise<void> {
		if (mergeBlocked) return;
		refusal = null;
		try {
			const joined = await join.mutateAsync({
				oldAlbumId: oldAlbum.id,
				newAlbumId: newAlbum.id,
				keepEditionOf: pair.editions_differ ? keep : null,
				expectedIdentityRevisions: {
					[oldAlbum.id]: oldAlbum.identity_revision,
					[newAlbum.id]: newAlbum.identity_revision
				}
			});
			if (currentAlbumId && joined.album_id !== currentAlbumId) {
				await goto(withBasePath(`/album/${encodeURIComponent(joined.album_id)}`));
			}
		} catch (error) {
			await settleRefusal(error);
		}
	}

	async function keepApart(): Promise<void> {
		refusal = null;
		try {
			await dismiss.mutateAsync({ oldAlbumId: oldAlbum.id, newAlbumId: newAlbum.id });
		} catch (error) {
			await settleRefusal(error);
		}
	}
</script>

{#snippet album(side: RetagPair['old_album'])}<a
		class="link font-semibold"
		href={withBasePath(`/album/${encodeURIComponent(side.id)}`)}>{side.title}</a
	>{/snippet}

<Card
	as="article"
	variant="bordered"
	padding="md"
	aria-label="Might be one album: {oldAlbum.title} and {newAlbum.title}"
>
	<p
		class="flex items-center gap-1.5 text-xs font-semibold tracking-wide text-base-content/60 uppercase"
	>
		<Combine class="h-3.5 w-3.5" aria-hidden="true" />Might be one album
	</p>
	<h3 class="mt-1 font-semibold">These look like one album retagged outside DroppedNeedle</h3>

	<div class="mt-2 space-y-2 text-sm text-base-content/80">
		{#if pair.reason === 'tracks_elsewhere'}
			<p>
				Some tracks of {@render album(oldAlbum)} were moved to another album earlier, and the rest were
				retagged as {@render album(newAlbum)}. DroppedNeedle can't tell an album split in two from
				an album renamed. If {newAlbum.title} is {oldAlbum.title} renamed, merge them: {oldAlbum.title}'s
				edition goes to {newAlbum.title} only.
			</p>
		{:else if pair.reason === 'replaced_hands_off'}
			<p>
				{@render album(oldAlbum)} lost its files in the same scan that {@render album(newAlbum)} appeared
				beside it. Nothing shows they are one album, so DroppedNeedle has left {newAlbum.title} alone.
			</p>
		{:else if pair.reason === 'custom_edition'}
			<p>
				{@render album(oldAlbum)} ({tracks(oldAlbum.track_count)}) looks like it was retagged as {@render album(
					newAlbum
				)} ({tracks(newAlbum.track_count)}).
			</p>
		{:else}
			<p>
				{@render album(oldAlbum)} ({tracks(oldAlbum.track_count)}) and {@render album(newAlbum)} ({tracks(
					newAlbum.track_count
				)}) have different editions chosen, so DroppedNeedle did not join them.
			</p>
		{/if}

		{#if oldAlbum.edition?.custom}
			<p>
				{oldAlbum.title} has its own track list (a custom edition). Merging drops it: it does not carry
				over and can't be put back.
			</p>
		{/if}

		{#if pair.editions_differ}
			<fieldset class="space-y-1.5 pt-1">
				<legend class="sr-only">Which edition to keep</legend>
				{#each [{ side: oldAlbum, fromOld: true }, { side: newAlbum, fromOld: false }] as choice (choice.side.id)}
					<label class="flex cursor-pointer items-center gap-2">
						<input
							type="radio"
							class="radio radio-sm"
							name={radioName}
							value={choice.side.id}
							bind:group={keep}
						/>
						<span>{keepLabel(choice.side, choice.fromOld)}</span>
					</label>
				{/each}
			</fieldset>
			{#if keep === oldAlbum.id && newAlbum.edition?.custom}
				<p>
					{newAlbum.title} has its own track list (a custom edition). Keeping {oldAlbum.title}'s
					edition drops it: it does not carry over and can't be put back.
				</p>
			{/if}
		{/if}

		<p class="text-base-content/60">
			Merging keeps {newAlbum.title} and moves {oldAlbum.title}'s favorite, plays and playlist
			entries to it.
		</p>
		{#if pair.reason === 'replaced_hands_off'}
			<p class="text-base-content/60">
				Plays of single tracks stay with the old tracks. Merged or kept apart, {newAlbum.title} stays
				left unmanaged until you choose "Manage it again".
			</p>
		{/if}
		{#if keepsOldEdition}
			<p class="text-base-content/60">
				If DroppedNeedle never wrote these files, the album is left unmanaged after the merge until
				you choose "Manage it again", which writes the tags of {keptEditionName}.
			</p>
		{/if}
	</div>

	{#if refusal}
		<p class="mt-3 text-sm text-warning" role="alert">{refusal}</p>
	{/if}

	<div class="mt-3 flex flex-wrap gap-2">
		<AsyncButton
			variant="primary"
			size="sm"
			disabled={mergeBlocked || dismiss.isPending}
			pending={join.isPending}
			onclick={merge}>Merge into one album</AsyncButton
		>
		<AsyncButton
			variant="ghost"
			size="sm"
			disabled={join.isPending}
			pending={dismiss.isPending}
			onclick={keepApart}>Keep them apart</AsyncButton
		>
	</div>
</Card>
