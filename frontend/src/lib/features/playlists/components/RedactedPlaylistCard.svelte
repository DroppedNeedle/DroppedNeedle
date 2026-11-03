<script lang="ts">
	import type { RedactedPlaylist } from '$lib/api/playlists';
	import { createDeletePlaylistV3Mutation } from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import { privatePlaylistLine } from '$lib/features/playlists/queries/playlistV3Adapter';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import { Lock, Trash2 } from 'lucide-svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		playlist: RedactedPlaylist;
		ondelete?: (playlistId: string) => void;
	}

	let { playlist, ondelete }: Props = $props();

	const deleteMutation = createDeletePlaylistV3Mutation();
	let confirmOpen = $state(false);

	let line = $derived(privatePlaylistLine(playlist));

	async function confirmDelete() {
		await deleteMutation.mutateAsync(playlist.id);
		toastStore.show({ message: 'Playlist deleted.', type: 'success' });
		ondelete?.(playlist.id);
	}
</script>

<!-- A private playlist the caller may not read: no name, no cover, only count
	 and owner. It links to the locked page. An admin may delete one that has no owner. -->
<Card as="article" padding="none" class="w-full shrink-0 select-none">
	<a
		href={withBasePath(`/playlists/${playlist.id}`)}
		class="block rounded-box focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-base-100"
		aria-label="Private playlist, {line}"
	>
		<figure
			class="aspect-square overflow-hidden relative flex items-center justify-center rounded-t-box bg-base-300/40 opacity-70"
		>
			<Lock class="h-10 w-10 text-base-content/30" />
		</figure>
		<div class="p-3 opacity-70">
			<h3 class="text-sm font-semibold italic text-base-content/60 line-clamp-1">
				Private playlist
			</h3>
			<p class="text-xs text-base-content/50 mt-0.5 line-clamp-2">{line}</p>
		</div>
	</a>
	{#if playlist.can_delete}
		<div class="flex justify-end px-3 pb-2.5">
			<IconButton
				size="sm"
				icon={Trash2}
				label="Delete private playlist with no owner"
				onclick={() => (confirmOpen = true)}
			/>
		</div>
		<ConfirmDialog
			bind:open={confirmOpen}
			title="Delete this playlist?"
			message="It has no owner. This cannot be undone."
			confirmLabel="Delete"
			tone="danger"
			onconfirm={confirmDelete}
		/>
	{/if}
</Card>
