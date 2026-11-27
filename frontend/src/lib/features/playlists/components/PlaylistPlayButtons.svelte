<!--
	Round Play and Shuffle for a playlist in a list: they read its songs,
	queue the playable ones and start. Both are off while it has no songs.
-->
<script lang="ts">
	import { fetchPlaylist, isRedactedPlaylist, type PlaylistSummary } from '$lib/api/playlists';
	import {
		playlistTrackToQueueItem,
		withPlayedFrom
	} from '$lib/features/player/playback/queueHelpers';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { Play, Shuffle } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		playlist: Pick<PlaylistSummary, 'id' | 'name' | 'track_count'>;
	}

	let { playlist }: Props = $props();

	let loading = $state<'play' | 'shuffle' | null>(null);
	let empty = $derived(playlist.track_count === 0);

	async function start(e: Event, shuffle: boolean) {
		e.preventDefault();
		e.stopPropagation();
		if (loading || empty) return;
		loading = shuffle ? 'shuffle' : 'play';
		try {
			const detail = await fetchPlaylist(playlist.id);
			if (isRedactedPlaylist(detail)) return;
			const items = detail.tracks
				.map(playlistTrackToQueueItem)
				.filter((item): item is NonNullable<typeof item> => item !== null);
			if (items.length === 0) {
				toastStore.show({
					message: "This playlist doesn't have anything playable yet.",
					type: 'info'
				});
				return;
			}
			playerStore.playQueue(
				withPlayedFrom(items, { kind: 'playlist', id: playlist.id }),
				0,
				shuffle
			);
		} catch {
			toastStore.show({ message: "Couldn't load that playlist.", type: 'error' });
		} finally {
			loading = null;
		}
	}
</script>

<div class="flex gap-1.5">
	<IconButton
		variant="primary"
		size="sm"
		icon={Play}
		label="Play {playlist.name}"
		class="[&_svg]:fill-current"
		loading={loading === 'play'}
		disabled={empty}
		onclick={(e: Event) => void start(e, false)}
	/>
	<IconButton
		variant="neutral"
		size="sm"
		icon={Shuffle}
		label="Shuffle {playlist.name}"
		loading={loading === 'shuffle'}
		disabled={empty}
		onclick={(e: Event) => void start(e, true)}
	/>
</div>
