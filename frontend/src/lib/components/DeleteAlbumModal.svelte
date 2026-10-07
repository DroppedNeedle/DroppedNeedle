<script lang="ts">
	import { removeLibraryAlbum } from '$lib/queries/library/LibraryMutations.svelte';

	interface Props {
		albumTitle: string;
		artistName: string;
		musicbrainzId: string;
		ondeleted: () => void | Promise<void>;
		onclose: () => void;
	}

	let { albumTitle, artistName, musicbrainzId, ondeleted, onclose }: Props = $props();

	let dialogEl: HTMLDialogElement | undefined = $state();
	let removing = $state(false);
	let error = $state<string | null>(null);
	let stopWanted = $state(true);
	const removal = removeLibraryAlbum();

	$effect(() => {
		if (dialogEl && musicbrainzId) {
			dialogEl.showModal();
		}
	});

	function handleClose() {
		dialogEl?.close();
		onclose();
	}

	async function handleRemove() {
		removing = true;
		error = null;

		try {
			await removal.mutateAsync({ mbid: musicbrainzId, stopWanted });
			await ondeleted();
		} catch (e) {
			error = e instanceof Error ? e.message : "Couldn't remove this album";
		} finally {
			removing = false;
		}
	}
</script>

<dialog bind:this={dialogEl} class="modal" onclose={handleClose}>
	<div class="modal-box max-w-md">
		<h3 class="text-lg font-bold">Remove Album</h3>
		<p class="py-4 text-base-content/70">
			Remove <span class="font-semibold text-base-content">{albumTitle}</span> by
			<span class="font-semibold text-base-content">{artistName}</span> from your library? The album's
			files move to the recycle bin, so you can get them back from there if you change your mind.
		</p>
		<label class="flex cursor-pointer items-start gap-3 rounded-box bg-base-200 p-3 text-sm">
			<input
				type="checkbox"
				class="checkbox checkbox-sm mt-0.5"
				bind:checked={stopWanted}
				disabled={removing}
			/>
			<span>
				<span class="font-semibold">Stop the Wanted watcher</span>
				<span class="mt-1 block text-base-content/65">
					Uncheck this to keep looking for a replacement after the album is removed.
				</span>
			</span>
		</label>

		{#if error}
			<div class="alert alert-error mt-3 text-sm">
				<span>{error}</span>
			</div>
		{/if}

		<div class="modal-action">
			<button class="btn btn-ghost" onclick={handleClose} disabled={removing}> Cancel </button>
			<button class="btn btn-error" onclick={handleRemove} disabled={removing}>
				{#if removing}
					<span class="loading loading-spinner loading-sm"></span>
					Removing...
				{:else}
					Remove
				{/if}
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>close</button>
	</form>
</dialog>
