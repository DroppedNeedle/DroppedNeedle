<!--
	What a playlist's missing songs wait for, for its owner and editors: the
	albums a press would ask for with the button that asks, the songs no
	album was found for, the album finder while it runs, and the Spotify
	import while it runs or after it failed. Shows nothing when none applies.
-->
<script lang="ts">
	import { Download } from 'lucide-svelte';
	import { getPlaylistRequestStateQuery } from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
	import { createRequestPlaylistMissingMutation } from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { applyEffects, playlistChanged } from '$lib/queries/effects';
	import { toastStore } from '$lib/stores/toast';
	import { createUuid } from '$lib/utils/uuid';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { importLine, pressMessage } from './requestStates';

	interface Props {
		playlistId: string;
	}

	let { playlistId }: Props = $props();

	const stateQuery = getPlaylistRequestStateQuery(
		() => playlistId,
		() => true
	);
	const pressMutation = createRequestPlaylistMissingMutation();

	const state = $derived(stateQuery.data);
	const albums = $derived(state?.requestable_albums ?? 0);
	const noAlbum = $derived(state?.no_album_songs ?? 0);
	const imported = $derived(importLine(state));
	const shown = $derived(albums > 0 || noAlbum > 0 || state?.finding_albums || imported !== null);

	// Once the album finder ends, read the playlist again so the albums it
	// found show their covers and links.
	let wasFinding = false;
	$effect(() => {
		const finding = state?.finding_albums ?? false;
		if (wasFinding && !finding) void applyEffects(playlistChanged(authStore.user?.id, playlistId));
		wasFinding = finding;
	});

	async function press() {
		try {
			const result = await pressMutation.mutateAsync({
				playlistId,
				idempotencyKey: createUuid()
			});
			toastStore.show({ message: pressMessage(result), type: 'success' });
		} catch (error) {
			toastStore.show({
				message:
					error instanceof Error && error.message ? error.message : "Couldn't ask for those albums",
				type: 'error'
			});
		}
	}
</script>

{#if shown}
	<Card variant="note" padding="sm">
		<div class="flex flex-wrap items-center gap-x-4 gap-y-3">
			<div class="flex min-w-0 flex-1 basis-48 flex-col gap-1 text-sm">
				{#if albums > 0}
					<p class="font-medium">{albums} {albums === 1 ? 'album' : 'albums'} to request</p>
				{/if}
				{#if noAlbum > 0}
					<p class="text-muted">
						{noAlbum}
						{noAlbum === 1 ? 'song has' : 'songs have'} no album found yet
					</p>
				{/if}
				{#if state?.finding_albums}
					<p class="text-muted">Finding albums...</p>
				{/if}
				{#if imported}
					<p class="text-muted">{imported}</p>
				{/if}
			</div>
			{#if albums > 0}
				<Button
					variant="outline"
					size="display-sm"
					icon={Download}
					loading={pressMutation.isPending}
					onclick={() => void press()}
				>
					Request {albums === 1 ? 'album' : `${albums} albums`}
				</Button>
			{/if}
		</div>
	</Card>
{/if}
