<!--
	The lyrics on their own, a sheet rising above the bottom of the page:
	the Listening Room's. The player bar opens them as a tab of its drawer
	instead. Escape closes it while focus is inside.
-->
<script lang="ts">
	import { X } from 'lucide-svelte';
	import { slide } from '$lib/ui/motion';
	import IconButton from '$lib/ui/IconButton.svelte';
	import LyricsView from '$lib/features/player/components/LyricsView.svelte';
	import type { LyricLine } from '$lib/features/player/queries/types';

	interface Props {
		open: boolean;
		lyricsText: string;
		lines?: LyricLine[];
		isSynced?: boolean;
		isLoading?: boolean;
		hasError?: boolean;
		currentTime?: number;
		trackName?: string;
		artistName?: string;
		onclose: () => void;
	}

	let { open = $bindable(), onclose, ...view }: Props = $props();

	function handleClose(): void {
		open = false;
		onclose();
	}
</script>

{#if open}
	<div
		class="pointer-events-none fixed inset-x-0 bottom-[90px] z-40 flex justify-center"
		role="dialog"
		aria-label="Lyrics"
		aria-modal="true"
		tabindex="-1"
		onkeydown={(e) => e.key === 'Escape' && handleClose()}
	>
		<div
			class="pointer-events-auto mx-4 flex max-h-[60vh] w-full max-w-lg flex-col overflow-hidden rounded-box border border-line bg-base-200 shadow-[0_24px_60px_-24px_rgba(0,0,0,0.6)]"
			transition:slide={{ duration: 250 }}
		>
			<div class="flex items-center justify-between border-b border-line py-1 pr-2 pl-5">
				<h3 class="font-mono text-[0.8125rem] font-bold tracking-[0.16em] uppercase">Lyrics</h3>
				<IconButton size="sm" icon={X} label="Close lyrics" onclick={handleClose} />
			</div>
			<LyricsView {...view} />
		</div>
	</div>
{/if}
