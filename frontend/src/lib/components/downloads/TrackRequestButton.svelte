<script lang="ts">
	import { Download } from 'lucide-svelte';

	import { requestTrack } from '$lib/queries/downloads/DownloadMutations.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	// orphan-track case (album not in library) is resolved by the backend
	interface Props {
		recordingMbid: string;
		trackTitle: string;
		artistName: string;
		albumMbid: string;
		albumTitle?: string | null;
		durationSeconds?: number | null;
		artistMbid?: string | null;
		releaseMbid?: string | null;
	}
	let {
		recordingMbid,
		trackTitle,
		artistName,
		albumMbid,
		albumTitle = null,
		durationSeconds = null,
		artistMbid = null,
		releaseMbid = null
	}: Props = $props();

	const request = requestTrack();
	let requested = $state(false);

	function handleClick() {
		request.mutate(
			{
				recording_mbid: recordingMbid,
				artist_name: artistName,
				track_title: trackTitle,
				album_title: albumTitle,
				duration_seconds: durationSeconds,
				release_group_mbid: albumMbid,
				artist_mbid: artistMbid,
				release_id: releaseMbid
			},
			{ onSuccess: () => (requested = true) }
		);
	}
</script>

<IconButton
	size="xs"
	icon={Download}
	label="Request this track"
	loading={request.isPending}
	disabled={requested}
	onclick={handleClick}
/>
