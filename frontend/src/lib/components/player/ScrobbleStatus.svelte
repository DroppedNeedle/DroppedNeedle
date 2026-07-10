<script lang="ts">
	/*
	 * ScrobbleStatus — Last.fm / ListenBrainz tracking indicator,
	 * same states as the previous player bar (tracking / scrobbled / error).
	 */
	import { scrobbleManager } from '$lib/stores/scrobble.svelte';
	import { Check, CircleX } from 'lucide-svelte';
</script>

{#if scrobbleManager.enabled && scrobbleManager.status !== 'idle'}
	<div class="tooltip tooltip-left" data-tip={scrobbleManager.tooltip}>
		{#if scrobbleManager.status === 'scrobbled'}
			<Check class="h-4 w-4 text-success" />
		{:else if scrobbleManager.status === 'error'}
			<CircleX class="h-4 w-4 text-error" />
		{:else}
			<span class="badge badge-info badge-sm gap-1 font-semibold">
				<span class="status status-md status-info"></span>
				Tracking
			</span>
		{/if}
	</div>
{/if}
