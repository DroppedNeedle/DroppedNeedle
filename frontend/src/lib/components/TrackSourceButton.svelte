<script lang="ts">
	import { Play } from 'lucide-svelte';
	import type { Snippet } from 'svelte';

	interface Props {
		available: boolean;
		sourceColor: string;
		onclick: () => void;
		ariaLabel: string;
		icon: Snippet;
	}

	let { available, sourceColor, onclick, ariaLabel, icon }: Props = $props();
</script>

{#if available}
	<button
		class="btn btn-sm rounded-lg gap-1.5 shrink-0 border-none max-sm:px-2 max-sm:gap-1"
		style="--btn-color: {sourceColor}; background-color: var(--btn-color); color: oklch(98% 0 0);"
		{onclick}
		aria-label={ariaLabel}
	>
		<Play class="h-4 w-4 fill-current" />
		{@render icon()}
	</button>
{:else}
	<span
		class="btn btn-sm btn-ghost rounded-lg shrink-0 pointer-events-none opacity-30 max-sm:px-2"
		aria-label="Not available"
	>
		—
	</span>
{/if}
