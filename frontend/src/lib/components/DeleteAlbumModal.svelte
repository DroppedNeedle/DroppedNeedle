<script lang="ts">
	import { getDownloadPolicyQuery } from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { removeLibraryAlbum } from '$lib/queries/library/LibraryMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';

	interface Props {
		albumTitle: string;
		artistName: string;
		musicbrainzId: string;
		ondeleted: () => void | Promise<void>;
		onclose: () => void;
	}

	let { albumTitle, artistName, musicbrainzId, ondeleted, onclose }: Props = $props();

	let open = $state(true);
	let removing = $state(false);
	let stopWanted = $state(true);
	const removal = removeLibraryAlbum();
	// How long the recycle bin keeps the files, from the download settings.
	const policyQuery = getDownloadPolicyQuery(() => authStore.isAdmin);
	const keptFor = $derived.by(() => {
		const days = policyQuery.data?.recycle_retention_days;
		return days ? ` for ${days} ${days === 1 ? 'day' : 'days'}` : '';
	});

	async function handleRemove() {
		removing = true;
		try {
			await removal.mutateAsync({ mbid: musicbrainzId, stopWanted });
		} catch (e) {
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
	<p class="text-base-content/70">
		Remove <span class="font-semibold text-base-content">{albumTitle}</span> by
		<span class="font-semibold text-base-content">{artistName}</span> from your library? Its files
		go to the recycle bin{keptFor}. Put them back from the album page.
	</p>
	<label class="mt-4 flex cursor-pointer items-start gap-3 rounded-box bg-base-200 p-3 text-sm">
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
</ConfirmDialog>
