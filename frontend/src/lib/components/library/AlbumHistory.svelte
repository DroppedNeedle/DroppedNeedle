<!--
	What DroppedNeedle did by itself to a library album: history moved to new
	files, tracks whose paths other music took, tracks replaced by new copies
	of the same recordings, albums joined into this one, and an artist taken
	from the tracks instead of "Unknown Artist". A curator can undo the fold
	of a leftover album and that artist change. Nothing shows while it loads
	or when there is nothing to tell.
-->
<script lang="ts">
	import { History, Undo2 } from 'lucide-svelte';

	import {
		getAlbumHistoryQuery,
		undoAlbumFold,
		undoArtistRepair,
		type AlbumHistoryEntry
	} from '$lib/queries/library/LibraryAlbumHistoryQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { formatRelativeTime } from '$lib/utils/formatting';

	interface Props {
		/** The local album the page shows. */
		albumId: string;
	}

	let { albumId }: Props = $props();

	const query = getAlbumHistoryQuery(
		() => authStore.user?.id,
		() => albumId
	);

	const undoFold = undoAlbumFold();
	const undoArtist = undoArtistRepair();
	let undoing = $state<{ id: string; artist: boolean } | null>(null);
	let confirming = $state(false);

	function askUndo(entry: AlbumHistoryEntry, id: string): void {
		undoing = { id, artist: entry.kind === 'artist_from_tracks' };
		confirming = true;
	}

	async function runUndo(): Promise<void> {
		if (!undoing) return;
		if (undoing.artist) {
			await undoArtist.mutateAsync(undoing.id);
			toastStore.show({ message: 'Artist change undone.', type: 'success' });
		} else {
			await undoFold.mutateAsync(undoing.id);
			toastStore.show({ message: 'Join undone.', type: 'success' });
		}
	}

	const MOVED = 'plays, favorites and playlist entries';

	function titles(entry: AlbumHistoryEntry): string {
		const quoted = entry.track_titles.map((title) => `"${title}"`).join(', ');
		const more = entry.track_count - entry.track_titles.length;
		return more > 0 ? `${quoted} and ${more} more` : quoted;
	}

	function trackList(entry: AlbumHistoryEntry): string {
		return entry.track_titles.length ? `: ${titles(entry)}.` : '.';
	}

	function single(entry: AlbumHistoryEntry): string | null {
		if (entry.track_count !== 1) return null;
		return entry.track_titles.length ? `"${entry.track_titles[0]}"` : 'A track';
	}

	function line(entry: AlbumHistoryEntry): string {
		const old = entry.other_album_title ?? 'Another album';
		const one = single(entry);
		switch (entry.kind) {
			case 'tracks_continued':
				return one
					? `${one} has a new file, and its ${MOVED} moved to it.`
					: `${entry.track_count} tracks have new files, and their ${MOVED} moved to them${trackList(entry)}`;
			case 'tracks_replaced':
				return one
					? `${one} was replaced by the edition's version, and its ${MOVED} moved to the new file.`
					: `${entry.track_count} tracks were replaced by the edition's version, and their ${MOVED} moved to the new files${trackList(entry)}`;
			case 'tracks_other_music':
				return one
					? `${one} was replaced by other music, which is now a new track. Its ${MOVED} stayed with the old track, which shows as missing.`
					: `${entry.track_count} tracks were replaced by other music, which came in as new tracks. Their ${MOVED} stayed with the old tracks, which show as missing${trackList(entry)}`;
			case 'tracks_new_copy':
				return one
					? `${one} was replaced by a new copy of the same recording.`
					: `${entry.track_count} tracks were replaced by new copies of the same recordings${trackList(entry)}`;
			case 'retag_joined':
				if (entry.by === 'you') return `You merged ${old} into this album.`;
				if (entry.by === 'person') return `An admin merged ${old} into this album.`;
				return `${old} was joined into this album after its files were retagged outside DroppedNeedle.`;
			case 'retag_half_left_alone':
				return `Left unmanaged like ${old}, whose files were being retagged as this album.`;
			case 'leftover_joined':
				return `${old}, left over from an earlier version as a separate album, was folded into this album.`;
			case 'artist_from_tracks':
				return 'The artist was taken from the tracks, replacing "Unknown Artist".';
		}
	}
</script>

<QueryBoundary {query} what="the album's history">
	{#snippet loading()}{/snippet}
	{#snippet empty()}{/snippet}
	{#snippet children(history)}
		<section class="mt-6" aria-labelledby="album-history-title">
			<div class="flex items-center gap-2">
				<History class="h-5 w-5 text-primary" aria-hidden="true" />
				<h2 id="album-history-title" class="text-xl font-bold">Album history</h2>
			</div>
			<ul class="mt-3 space-y-2">
				{#each history.items as entry, index (index)}
					{@const at = new Date(entry.at)}
					<li
						class="flex flex-col gap-1 rounded-box bg-base-200/50 px-4 py-3 text-sm sm:flex-row sm:items-baseline sm:justify-between sm:gap-4"
					>
						<span class="min-w-0 break-words">{line(entry)}</span>
						<span class="flex shrink-0 items-center gap-3">
							<time
								class="text-xs text-base-content/60"
								datetime={at.toISOString()}
								title={at.toLocaleString()}>{formatRelativeTime(at)}</time
							>
							{#if entry.undo_id && authStore.isTrusted}
								{@const id = entry.undo_id}
								<Button variant="outline" size="xs" icon={Undo2} onclick={() => askUndo(entry, id)}
									>Undo</Button
								>
							{/if}
						</span>
					</li>
				{/each}
			</ul>
		</section>
	{/snippet}
</QueryBoundary>

<ConfirmDialog
	bind:open={confirming}
	title={undoing?.artist ? 'Undo this artist change?' : 'Undo this join?'}
	message={undoing?.artist
		? 'The album goes back to "Unknown Artist", and this change is not made again.'
		: 'Plays, favorites and playlist entries stay on this album.'}
	confirmLabel="Undo"
	pending={undoFold.isPending || undoArtist.isPending}
	onconfirm={runUndo}
/>
