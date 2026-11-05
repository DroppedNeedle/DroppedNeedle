<!--
	An album's library state on a search result: IN LIBRARY or REQUESTED.
	For an admin it is a button that turns into REMOVE on hover or focus and
	opens the remove dialog.
-->
<script lang="ts">
	import { Check, Clock, Trash2 } from 'lucide-svelte';
	import DeleteAlbumModal from '$lib/features/album/components/DeleteAlbumModal.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';

	interface Props {
		status: 'library' | 'requested';
		musicbrainzId: string;
		albumTitle: string;
		artistName: string;
		ondeleted?: () => void;
	}

	let { status, musicbrainzId, albumTitle, artistName, ondeleted }: Props = $props();

	let removing = $state(false);

	const label = $derived(status === 'library' ? 'In library' : 'Requested');

	function open(event: Event) {
		event.preventDefault();
		event.stopPropagation();
		removing = true;
	}
</script>

{#snippet chip()}
	<StatusChip
		tone={status === 'library' ? 'action' : 'warning'}
		size="xs"
		icon={status === 'library' ? Check : Clock}
		class="search-chip__state">{label}</StatusChip
	>
{/snippet}

{#if authStore.isAdmin}
	<button
		type="button"
		class="search-chip rounded-full focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus"
		aria-label={status === 'library' ? 'Remove from library' : 'Remove request'}
		onclick={open}
	>
		{@render chip()}
		<StatusChip tone="error" size="xs" icon={Trash2} class="search-chip__remove">Remove</StatusChip>
	</button>
{:else}
	{@render chip()}
{/if}

{#if removing}
	<DeleteAlbumModal
		{albumTitle}
		{artistName}
		{musicbrainzId}
		ondeleted={() => {
			removing = false;
			ondeleted?.();
		}}
		onclose={() => (removing = false)}
	/>
{/if}

<style>
	.search-chip {
		display: grid;
	}
	.search-chip > :global(*) {
		grid-area: 1 / 1;
	}
	.search-chip :global(.search-chip__remove),
	.search-chip:is(:hover, :focus-visible) :global(.search-chip__state) {
		visibility: hidden;
	}
	.search-chip:is(:hover, :focus-visible) :global(.search-chip__remove) {
		visibility: visible;
	}
</style>
