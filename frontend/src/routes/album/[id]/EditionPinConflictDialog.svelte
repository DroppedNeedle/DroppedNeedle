<script lang="ts">
	import { Disc3 } from 'lucide-svelte';

	import {
		clearLocalAlbumEditionPin,
		setLocalAlbumEditionPin
	} from '$lib/queries/albums/EditionQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { LibraryAlbumSummary } from '$lib/types';

	interface Props {
		releaseMbid: string | null;
		localCopies: LibraryAlbumSummary[];
		onrefresh: () => void;
		onclose: () => void;
	}

	let { releaseMbid, localCopies, onrefresh, onclose }: Props = $props();

	let dialogEl = $state<HTMLDialogElement | null>(null);
	let applyingId = $state<string | null>(null);

	const setPin = setLocalAlbumEditionPin();
	const clearPin = clearLocalAlbumEditionPin();

	export function showModal() {
		dialogEl?.showModal();
	}

	function close() {
		dialogEl?.close();
	}

	function handleDialogClose() {
		applyingId = null;
		onclose();
	}

	async function applyToCopy(copy: LibraryAlbumSummary) {
		applyingId = copy.id;
		try {
			const userId = authStore.user?.id;
			const rgMbid = copy.musicbrainz_release_group_id ?? '';
			if (releaseMbid === null) {
				await clearPin.mutateAsync({ userId, localId: copy.id, rgMbid });
				toastStore.show({ message: 'Edition back to automatic.', type: 'success' });
			} else {
				await setPin.mutateAsync({ userId, localId: copy.id, rgMbid, releaseMbid });
				toastStore.show({ message: `Edition pinned for "${copy.title}".`, type: 'success' });
			}
			onrefresh();
			close();
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Could not change the edition',
				type: 'error'
			});
		} finally {
			applyingId = null;
		}
	}
</script>

<dialog bind:this={dialogEl} class="modal" onclose={handleDialogClose}>
	<div class="modal-box">
		<h3 class="text-lg font-bold">Which copy should this edition apply to?</h3>
		<p class="py-3 text-sm text-base-content/70">
			This MusicBrainz release matches more than one album in your library, so the edition
			{releaseMbid === null ? 'reset to Automatic' : 'pin'} needs a single copy to land on.
		</p>
		<ul class="space-y-2">
			{#each localCopies as copy (copy.id)}
				<li>
					<button
						type="button"
						class="flex w-full items-center gap-3 rounded-box border border-base-content/10 bg-base-100 px-3 py-2 text-left text-sm transition-colors hover:border-primary/35"
						disabled={applyingId !== null}
						onclick={() => void applyToCopy(copy)}
					>
						<Disc3 class="h-5 w-5 shrink-0 text-base-content/40" />
						<span class="min-w-0 flex-1">
							<span class="block truncate font-medium">{copy.title}</span>
							<span class="block truncate text-xs text-base-content/50">
								{copy.artist_name} · {copy.track_count}
								{copy.track_count === 1 ? 'track' : 'tracks'}
							</span>
						</span>
						{#if applyingId === copy.id}
							<span class="loading loading-spinner loading-xs"></span>
						{/if}
					</button>
				</li>
			{/each}
		</ul>
		<div class="modal-action">
			<form method="dialog">
				<button class="btn btn-ghost">Cancel</button>
			</form>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>close</button>
	</form>
</dialog>
