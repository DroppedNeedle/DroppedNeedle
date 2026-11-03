<script lang="ts">
	import { getLibraryManagementSettingsQuery } from '$lib/features/library-management/queries/LibraryManagementQueries.svelte';
	import { isStaleRevision } from '$lib/api/errors';
	import { useQueryClient } from '@tanstack/svelte-query';
	import {
		getLibraryAlbumDetailQuery,
		getLibraryAlbumDetailQueryOptions,
		getLibraryAlbumStatusQuery
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import { createRemoveLibraryAlbumMutation } from '$lib/features/library/queries/LibraryMutations.svelte';
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
	const removal = createRemoveLibraryAlbumMutation();
	// Callers may know an album only by its release group: the status names the
	// local album, whose detail gives the revision the removal is checked
	// against and what this dialog shows.
	const status = getLibraryAlbumStatusQuery(() => musicbrainzId);
	const detail = getLibraryAlbumDetailQuery(() =>
		status.data?.in_library ? status.data.album_id : ''
	);
	const client = useQueryClient();
	const shownTitle = $derived(detail.data?.title ?? albumTitle);
	const shownArtist = $derived(detail.data?.artist_name ?? artistName);
	const countOf = (tracks: number) => `${tracks} ${tracks === 1 ? 'track' : 'tracks'}`;
	const shownCopy = $derived.by(() => {
		if (!detail.data) return '';
		const format = detail.data.format?.toUpperCase();
		return `${format ? `${format}, ` : ''}${countOf(detail.data.track_count)}`;
	});
	const copies = $derived(new Set(status.data?.tracks.map((track) => track.album_id)).size);
	// How long the recycle bin keeps the files. Settings that cannot be read
	// open with the starting values, so they say nothing here.
	const settingsQuery = getLibraryManagementSettingsQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const keptFor = $derived.by(() => {
		const settings = settingsQuery.data;
		const days = settings?.settings_unreadable ? undefined : settings?.recycle_retention_days;
		return days ? ` for ${days} ${days === 1 ? 'day' : 'days'}` : '';
	});

	// A refused removal reads the album again, so the next try sends the new
	// revision, and says what it holds now.
	async function whatChanged(refusal: string): Promise<string> {
		const { data, error } = await detail.refetch();
		if (error || !data) return refusal;
		return `${refusal} It is now ${data.title} by ${data.artist_name}, ${countOf(data.track_count)}.`;
	}

	// After a failed read a click reads again and stops there: nothing goes
	// until the person has seen what would.
	async function readAgain(): Promise<never> {
		const current = (await status.refetch()).data;
		if (!current?.in_library) throw new Error("Couldn't read this album");
		await client.fetchQuery(
			getLibraryAlbumDetailQueryOptions(authStore.user?.id, current.album_id)
		);
		throw new Error('Check the album, then remove it.');
	}

	async function handleRemove() {
		removing = true;
		try {
			const album = detail.data ?? (await readAgain());
			await removal.mutateAsync({
				mbid: musicbrainzId,
				albumId: album.id,
				expectedRevision: album.row_revision,
				expectedTrackCount: album.track_count,
				stopWanted
			});
		} catch (e) {
			if (isStaleRevision(e)) throw new Error(await whatChanged(e.message), { cause: e });
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
	pending={status.isLoading || detail.isLoading}
	onconfirm={handleRemove}
	oncancel={onclose}
>
	<p class="text-base-content/70">
		Remove <span class="font-semibold text-base-content">{shownTitle}</span> by
		<span class="font-semibold text-base-content">{shownArtist}</span> from your library? Its files
		go to the recycle bin{keptFor}. Put them back from the album page.
	</p>
	{#if shownCopy}
		<p class="mt-2 text-sm text-base-content/70">
			{#if copies > 1}You hold {copies} copies. This one goes:{/if}
			{shownCopy}
		</p>
	{/if}
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
