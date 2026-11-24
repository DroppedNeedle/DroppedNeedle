<!--
	Keep in sync, for a playlist imported from Spotify or Liked Songs, in one
	slim row: the owner's switch, what a synced song is requested as (the
	song or its whole album), and one line saying how the sync stands. An
	editor sees the line only, while sync is on. Shows nothing on other
	playlists.
-->
<script lang="ts">
	import { CircleCheck, Clock, TriangleAlert } from 'lucide-svelte';
	import { getPlaylistSyncQuery } from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
	import { createSetPlaylistSyncMutation } from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import Card from '$lib/ui/Card.svelte';
	import SegmentedChoice from '$lib/ui/SegmentedChoice.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { refusalText, syncLine } from './syncStates';

	interface Props {
		playlistId: string;
		/** The owner switches sync; an editor only reads it. */
		isOwner: boolean;
	}

	let { playlistId, isOwner }: Props = $props();

	const syncQuery = getPlaylistSyncQuery(
		() => playlistId,
		() => true
	);
	const setSync = createSetPlaylistSyncMutation();

	const sync = $derived(syncQuery.data);
	const line = $derived(sync ? syncLine(sync, isOwner) : null);
	const shown = $derived(Boolean(sync?.syncable) && (isOwner || Boolean(sync?.enabled)));
	const locked = $derived(setSync.isPending || Boolean(sync?.spotify_off && !sync.enabled));

	/** What each new song is requested as. */
	const REQUEST: { value: 'songs' | 'albums'; label: string }[] = [
		{ value: 'songs', label: 'Songs' },
		{ value: 'albums', label: 'Whole albums' }
	];

	async function save(enabled: boolean, wholeAlbums: boolean) {
		try {
			await setSync.mutateAsync({ playlistId, enabled, wholeAlbums });
		} catch (error) {
			toastStore.show({ message: refusalText(error, "Couldn't change the sync"), type: 'error' });
		}
	}
</script>

{#if shown && sync}
	<Card variant="bordered" padding="sm">
		<div class="sync-band">
			{#if isOwner}
				<ToggleRow
					leading
					label="Keep in sync"
					disabled={locked}
					bind:checked={() => sync.enabled, (on) => void save(on, sync.whole_albums)}
				/>
				{#if sync.enabled}
					<div class="sync-band__request">
						<span class="text-sm text-muted" aria-hidden="true">Request</span>
						<SegmentedChoice
							label="Request"
							options={REQUEST}
							value={sync.whole_albums ? 'albums' : 'songs'}
							disabled={locked}
							onchange={(asked) => void save(true, asked === 'albums')}
						/>
					</div>
				{/if}
			{/if}
			{#if line}
				<p class="sync-band__line" role="status">
					{#if line.tone === 'ok'}
						<CircleCheck class="size-4 shrink-0 text-success" aria-hidden="true" />
					{:else if line.tone === 'wait'}
						<Clock class="size-4 shrink-0 text-muted" aria-hidden="true" />
					{:else}
						<TriangleAlert class="size-4 shrink-0 text-warning" aria-hidden="true" />
					{/if}
					<span>
						{isOwner ? '' : 'Synced with Spotify. '}{line.text}
						{#if line.profile}
							<a href={withBasePath('/profile')} class="link">Open Profile</a>
						{/if}
					</span>
				</p>
			{:else if isOwner && !sync.enabled}
				<p class="text-sm text-muted">
					Add new songs from Spotify here and request the missing ones.
				</p>
			{/if}
		</div>
	</Card>
{/if}

<style>
	/* one slim row; on a phone the pieces stack, each on a line of its own */
	.sync-band {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.5rem;
	}
	.sync-band__request {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}
	.sync-band__line {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.875rem;
	}
	@media (min-width: 768px) {
		.sync-band {
			flex-direction: row;
			align-items: center;
			gap: 1.5rem;
		}
		.sync-band__line {
			margin-left: auto;
			text-align: right;
		}
	}
</style>
