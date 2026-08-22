<script lang="ts">
	import { resolve } from '$app/paths';
	import { requestAlbum } from '$lib/queries/downloads/DownloadMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { integrationStore } from '$lib/stores/integration';

	// inLibrary/requested are derived by the album page from GET /library/albums/{mbid}/status and passed in
	interface Props {
		releaseGroupMbid: string;
		title: string;
		artistName: string;
		year?: number | null;
		artistMbid?: string | null;
		inLibrary?: boolean;
		requested?: boolean;
		class?: string;
	}
	let {
		releaseGroupMbid,
		title,
		artistName,
		year = null,
		artistMbid = null,
		inLibrary = false,
		requested = false,
		class: klass = ''
	}: Props = $props();

	const request = requestAlbum();
	const configured = $derived($integrationStore.download_client);
	const isAdmin = $derived(authStore.isAdmin);

	let justRequested = $state(false);
	const isRequested = $derived(requested || justRequested);

	function handleClick() {
		request.mutate(
			{
				release_group_mbid: releaseGroupMbid,
				artist_name: artistName,
				album_title: title,
				year,
				artist_mbid: artistMbid
			},
			{ onSuccess: () => (justRequested = true) }
		);
	}
</script>

{#if inLibrary}
	<button class="btn btn-success {klass}" disabled>In Library</button>
{:else if isRequested}
	<button class="btn btn-info {klass}" disabled>Requested</button>
{:else if !configured}
	{#if isAdmin}
		<a href={resolve('/settings?tab=download-client')} class="btn btn-warning {klass}"
			>Configure Download Client</a
		>
	{:else}
		<button
			class="btn btn-ghost {klass}"
			disabled
			title="Contact your admin to configure the download client."
		>
			Downloads Unavailable
		</button>
	{/if}
{:else}
	<button class="btn btn-primary {klass}" onclick={handleClick} disabled={request.isPending}>
		{#if request.isPending}
			<span class="loading loading-spinner loading-sm"></span>
			Requesting…
		{:else}
			Request
		{/if}
	</button>
{/if}
