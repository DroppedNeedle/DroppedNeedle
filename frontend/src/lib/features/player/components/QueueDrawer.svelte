<!--
	The player's drawer opened on its Queue tab, for a page with its own
	queue button (the Listening Room). Each opening starts on Queue.
-->
<script lang="ts">
	import PlayerDrawer from '$lib/features/player/components/PlayerDrawer.svelte';
	import type { PlayerDrawerTab } from '$lib/features/player/state/playerDrawer.svelte';

	interface Props {
		open: boolean;
		onclose: () => void;
	}

	let { open = $bindable(), onclose }: Props = $props();

	let tab = $state<PlayerDrawerTab>('queue');

	$effect.pre(() => {
		if (open) tab = 'queue';
	});
</script>

<PlayerDrawer bind:open bind:tab {onclose} />
