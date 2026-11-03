<script lang="ts">
	import NowPlayingWidget from '$lib/features/player/components/NowPlayingWidget.svelte';
	import { nowPlayingMerged } from '$lib/features/player/state/nowPlayingMerged.svelte';

	const sessions = $derived(nowPlayingMerged.sessions);
</script>

<NowPlayingWidget {sessions} />
