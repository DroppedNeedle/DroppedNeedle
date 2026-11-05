<!--
	The equalizer on its own, floating above the bottom right: the Listening
	Room's. The player bar opens it as a tab of its drawer instead. A click
	outside or Escape closes it.
-->
<script lang="ts">
	import { X } from 'lucide-svelte';
	import { fly } from '$lib/ui/motion';
	import IconButton from '$lib/ui/IconButton.svelte';
	import EqView from '$lib/features/player/components/EqView.svelte';

	let { open = $bindable(), onclose }: { open: boolean; onclose: () => void } = $props();

	function handleClose(): void {
		open = false;
		onclose();
	}
</script>

{#if open}
	<button
		type="button"
		class="fixed inset-0 z-60 bg-transparent"
		onclick={handleClose}
		aria-label="Close equalizer"
		tabindex="-1"
	></button>

	<div
		class="fixed right-4 bottom-24.5 z-70 w-120 max-w-[calc(100vw-2rem)] rounded-box border border-line bg-base-200 shadow-[0_24px_60px_-24px_rgba(0,0,0,0.6)]"
		role="dialog"
		aria-label="Equalizer"
		tabindex="-1"
		transition:fly={{ y: 20, duration: 200 }}
		onkeydown={(e) => e.key === 'Escape' && handleClose()}
	>
		<div class="flex items-center justify-between border-b border-line py-1 pr-2 pl-5">
			<h3 class="font-mono text-[0.8125rem] font-bold tracking-[0.16em] uppercase">Equalizer</h3>
			<IconButton size="sm" icon={X} label="Close equalizer" onclick={handleClose} />
		</div>
		<EqView />
	</div>
{/if}
