<script lang="ts">
	import { isStaleRevision } from '$lib/api/errors';
	import { createRemoveLibraryAlbumMutation } from '$lib/features/library/queries/LibraryMutations.svelte';
	import type { AlbumDetailView } from '$lib/features/library/queries/types';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';

	interface Props {
		album: AlbumDetailView;
		/** Re-reads the album the dialog's revisions came from, after a stale refusal. */
		refetch?: () => Promise<unknown>;
		ondeleted: () => void | Promise<void>;
		onclose: () => void;
	}

	let { album, refetch, ondeleted, onclose }: Props = $props();

	let open = $state(true);
	let removing = $state(false);
	// Off by default: the files go to the recycle bin, where they can be put back.
	let deleteFiles = $state(false);
	const removal = createRemoveLibraryAlbumMutation();
	// Every streamable track has one file, so this count is the number
	// of files the removal moves.
	const fileCount = $derived(album.track_count);
	const hasFiles = $derived(fileCount > 0);
	const filesLabel = $derived(`${fileCount} ${fileCount === 1 ? 'file' : 'files'}`);

	async function handleRemove(): Promise<void> {
		removing = true;
		try {
			await removal.mutateAsync({
				mbid: album.release_group_mbid ?? album.id,
				albumId: album.id,
				expectedRevision: album.row_revision,
				expectedListingRevision: album.listing_revision,
				stopWanted: true,
				deleteFiles
			});
		} catch (e) {
			if (isStaleRevision(e)) {
				await refetch?.();
				throw new Error(
					'The album changed since this page was read. It has been refreshed — check it, then remove it.',
					{ cause: e }
				);
			}
			throw new Error(e instanceof Error && e.message ? e.message : "Couldn't remove this album", {
				cause: e
			});
		} finally {
			removing = false;
		}
		await ondeleted();
	}
</script>

<ConfirmDialog
	bind:open
	title="Remove Album"
	confirmLabel="Remove"
	tone="danger"
	onconfirm={handleRemove}
	oncancel={onclose}
>
	{#if hasFiles}
		<p class="text-muted">
			Remove <span class="font-semibold text-base-content">{album.title}</span> by
			<span class="font-semibold text-base-content"
				>{album.artist_name || 'Unknown album artist'}</span
			>
			from your library? Its {filesLabel} will be moved to the recycle bin, where you can put them back.
			The Wanted watch for this album stops.
		</p>
		<label class="mt-4 flex cursor-pointer items-start gap-3 rounded-box bg-base-200 p-3 text-sm">
			<input
				type="checkbox"
				class="checkbox checkbox-sm mt-0.5"
				bind:checked={deleteFiles}
				disabled={removing}
			/>
			<span>
				<span class="font-semibold">Delete the files instead of moving them to the recycle bin</span
				>
				<span class="mt-1 block text-muted"
					>The files are gone for good and cannot be put back.</span
				>
			</span>
		</label>
	{:else}
		<p class="text-muted">
			Remove <span class="font-semibold text-base-content">{album.title}</span> by
			<span class="font-semibold text-base-content"
				>{album.artist_name || 'Unknown album artist'}</span
			>
			from your library? This album has no files on disk. Removing it clears its rows. The Wanted watch
			for this album stops.
		</p>
	{/if}
</ConfirmDialog>
