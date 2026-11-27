<script lang="ts">
	import type { RedactedPlaylist } from '$lib/api/playlists';
	import { createDeletePlaylistV3Mutation } from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import { privatePlaylistLine } from '$lib/features/playlists/queries/playlistV3Adapter';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import { Lock, Trash2 } from 'lucide-svelte';
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
	 and owner. It links to the locked page. An admin may delete one that has no
	 owner, here, as the locked page has no delete. It has no Play or Shuffle:
	 its tracks cannot be read. -->
<article class="pl-locked">
	<div class="pl-locked__tile" aria-hidden="true">
		<Lock class="size-5" />
	</div>
	<a
		href={withBasePath(`/playlists/${playlist.id}`)}
		class="pl-locked__link"
		aria-label="Private playlist, {line}"
	>
		<h3 class="pl-locked__name">Private playlist</h3>
		<p class="pl-locked__meta">{line}</p>
	</a>
	{#if playlist.can_delete}
		<div class="pl-locked__actions">
			<IconButton
				variant="danger-ghost"
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
			icon={Trash2}
			onconfirm={confirmDelete}
		/>
	{/if}
</article>

<style>
	.pl-locked {
		position: relative;
		display: flex;
		align-items: center;
		gap: 0.75rem;
		min-width: 0;
		padding: 0.375rem;
		border: 1px dashed var(--dn-line);
		border-radius: var(--radius-box);
		transition: border-color 150ms;
	}
	.pl-locked:hover,
	.pl-locked:focus-within {
		border-color: var(--dn-edge);
	}

	/* where the cover goes: a quiet tile with the lock */
	.pl-locked__tile {
		display: grid;
		flex: none;
		place-items: center;
		width: 3.25rem;
		aspect-ratio: 1;
		border-radius: 0.375rem;
		background-color: var(--color-base-300);
		color: var(--dn-muted);
	}

	.pl-locked__link {
		display: flex;
		min-width: 0;
		flex: 1;
		flex-direction: column;
		gap: 0.25rem;
	}
	.pl-locked__link::after {
		content: '';
		position: absolute;
		inset: 0;
		border-radius: var(--radius-box);
	}
	.pl-locked__link:focus-visible {
		outline: none;
	}
	.pl-locked__link:focus-visible::after {
		outline: 2px solid var(--dn-focus);
		outline-offset: 2px;
	}

	.pl-locked__name {
		font-size: 0.9375rem;
		font-weight: 600;
		line-height: 1.25;
		color: var(--dn-muted);
	}

	.pl-locked__meta {
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-overflow: ellipsis;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--dn-muted);
	}

	.pl-locked__actions {
		position: relative;
		z-index: 1;
	}
</style>
