<script lang="ts">
	import EqControls from '$lib/components/player/EqControls.svelte';
	import { X } from 'lucide-svelte';
	import { fly } from 'svelte/transition';

	let { open = $bindable(), onclose }: { open: boolean; onclose: () => void } = $props();

	function handleClose(): void {
		open = false;
		onclose();
	}

	function handleKeydown(e: KeyboardEvent): void {
		if (e.key === 'Escape') {
			handleClose();
		}
	}
</script>

{#if open}
	<button
		class="fixed inset-0 z-60 bg-transparent"
		onclick={handleClose}
		aria-label="Close equalizer"
		tabindex="-1"
	></button>

	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="fixed bottom-24.5 right-4 z-70 w-120 max-w-[calc(100vw-2rem)] overscroll-contain
			   rounded-box bg-base-300 shadow-[0_-8px_40px_rgba(0,0,0,0.45)] border border-base-content/5"
		transition:fly={{ y: 20, duration: 200 }}
		onkeydown={handleKeydown}
	>
		<div class="flex justify-end px-2 pt-2 -mb-1">
			<button
				class="btn btn-ghost btn-xs btn-circle opacity-60 hover:opacity-100"
				onclick={handleClose}
				aria-label="Close equalizer"
			>
				<X class="h-3.5 w-3.5" />
			</button>
		</div>

		<div class="px-4 pb-4">
			<EqControls />
		</div>
	</div>
{/if}
