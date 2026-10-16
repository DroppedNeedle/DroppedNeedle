<!--
	An album library management leaves alone: why, since when, and the one
	"Manage it again" button. A person's own choice lifts the exclusion; a
	hold the app set hands the album back.
-->
<script lang="ts">
	import { CirclePause } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { LibraryAlbumDetail } from '$lib/types';
	import { reenableAlbumManagement } from '$lib/queries/library/LibraryCatalogMutations.svelte';
	import { getAlbumEditionsQuery } from '$lib/queries/albums/EditionQueries.svelte';
	import {
		handBackLibraryAlbumMutation,
		handBackRefusalMessage
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		album: LibraryAlbumDetail;
	}

	let { album }: Props = $props();

	const reenableManagement = reenableAlbumManagement();
	const handBack = handBackLibraryAlbumMutation();
	const hold = $derived(album.management_hold ?? null);
	// A hold the app set by itself, as opposed to the person's own choice.
	const steppedBack = $derived(hold !== null && hold.kind !== 'your_choice');
	const editionsQuery = getAlbumEditionsQuery(
		() => authStore.user?.id,
		() => album.musicbrainz_release_group_id ?? '',
		() => steppedBack && Boolean(album.musicbrainz_release_group_id)
	);
	const editionTitle = $derived(
		editionsQuery.data?.items.find((item) => item.release_mbid === album.musicbrainz_release_id)
			?.title ?? null
	);
	const holdCopy = $derived.by((): { heading: string; body: string } | null => {
		if (!hold) return null;
		const since = new Date(hold.held_at).toLocaleDateString(undefined, { dateStyle: 'medium' });
		switch (hold.kind) {
			case 'files_changed':
				return {
					heading: 'These files were changed outside DroppedNeedle',
					body: `DroppedNeedle noticed on ${since} and stopped retagging and renaming this album. Its edition, plays, favorites and playlist entries are as they were.`
				};
			case 'retag_followed':
				return {
					heading: 'This album was retagged outside DroppedNeedle',
					body: `Its plays, favorites and playlist entries are as they were. DroppedNeedle has left the files alone since ${since}.`
				};
			case 'run_undone':
				return {
					heading: "You undid DroppedNeedle's changes to this album",
					body: `It has left the files alone since ${since}.`
				};
			case 'replaced_hands_off':
				return {
					heading: 'These files may be your retag of another album',
					body: `They appeared on ${since} next to an album DroppedNeedle was not managing, in the same scan that saw that album's files go. DroppedNeedle has left them alone in case you retagged them by hand.`
				};
			case 'your_choice':
				return {
					heading: 'You chose to leave this album unmanaged',
					body: 'DroppedNeedle does not retag or rename its files.'
				};
			default:
				return { heading: 'This album is left unmanaged', body: '' };
		}
	});
	const handBackNote = $derived(
		album.musicbrainz_release_id
			? `DroppedNeedle will retag and rename these files from ${editionTitle ?? 'its edition'}. Changes made outside DroppedNeedle are overwritten. The run can be undone from the operations page.`
			: 'DroppedNeedle will manage this album again once it has an edition.'
	);
	// The files could not be read (a share that is not mounted): said here,
	// with the same button to try again.
	const handBackRefusal = $derived(handBackRefusalMessage(handBack.error));
</script>

{#if hold && holdCopy}
	<Card as="section" variant="bordered" padding="md" aria-label="Left unmanaged">
		<div class="flex flex-wrap items-start gap-3 sm:flex-nowrap">
			<div
				class="grid size-9 shrink-0 place-items-center rounded-xl bg-warning/10 text-warning"
				aria-hidden="true"
			>
				<CirclePause class="h-5 w-5" />
			</div>
			<div class="min-w-0 flex-1">
				<p class="text-[0.66rem] font-extrabold tracking-[0.12em] text-base-content/55 uppercase">
					Left unmanaged
				</p>
				<h3 class="hero-title text-lg font-bold">{holdCopy.heading}</h3>
				{#if holdCopy.body}<p class="text-xs text-base-content/60">{holdCopy.body}</p>{/if}
				<div class="mt-3 flex flex-wrap items-center gap-3">
					{#if hold.kind === 'your_choice'}
						<AsyncButton
							variant="outline"
							size="sm"
							disabled={hold.exclusion_revision == null}
							pending={reenableManagement.isPending}
							onclick={() =>
								hold.exclusion_revision != null &&
								reenableManagement
									.mutateAsync({ albumId: album.id, expectedRevision: hold.exclusion_revision })
									.catch(() => undefined)}>Manage it again</AsyncButton
						>
					{:else}
						<AsyncButton
							variant="outline"
							size="sm"
							disabled={hold.handing_back}
							pending={handBack.isPending}
							onclick={() => handBack.mutateAsync({ albumId: album.id }).catch(() => undefined)}
							>{hold.handing_back ? 'Being handed back' : 'Manage it again'}</AsyncButton
						>
						<p class="max-w-xl flex-1 text-xs text-base-content/60">{handBackNote}</p>
					{/if}
				</div>
				{#if handBackRefusal}<div class="mt-2 text-sm text-warning" role="status">
						{handBackRefusal}
					</div>{/if}
			</div>
		</div>
	</Card>
{/if}
