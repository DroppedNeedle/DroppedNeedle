<!-- The player's own toast (`playbackToast`), above the player bar while it shows. -->
<script lang="ts">
	import { Info, TriangleAlert, X } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
</script>

{#if playbackToast.visible}
	<div
		class="droppedneedle-playback-toast fixed z-50 left-1/2 -translate-x-1/2 transition-all duration-300"
		class:droppedneedle-playback-toast--player={playerStore.isPlayerVisible}
	>
		<div
			class="alert {playbackToast.type === 'error'
				? 'alert-error'
				: playbackToast.type === 'warning'
					? 'alert-warning'
					: 'alert-info'} shadow-lg px-4 py-2 min-w-64 max-w-md"
		>
			{#if playbackToast.type === 'error'}
				<X class="h-5 w-5 shrink-0" />
			{:else if playbackToast.type === 'warning'}
				<TriangleAlert class="h-5 w-5 shrink-0" />
			{:else}
				<Info class="h-5 w-5 shrink-0" />
			{/if}
			<span class="text-sm">{playbackToast.message}</span>
			<IconButton size="xs" icon={X} label="Dismiss" onclick={() => playbackToast.dismiss()} />
		</div>
	</div>
{/if}
