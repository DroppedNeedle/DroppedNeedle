<script lang="ts">
	import { Play } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import Button from '$lib/ui/Button.svelte';

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
	<!-- Each source keeps its own colour, which the kit's variants do not cover. -->
	<Button
		size="sm"
		icon={Play}
		class="shrink-0 [&_svg]:fill-current"
		style="--btn-color: {sourceColor}; background-color: var(--btn-color); color: oklch(98% 0 0);"
		{onclick}
		aria-label={ariaLabel}
	>
		{@render icon()}
	</Button>
{:else}
	<span
		class="inline-flex h-8 min-w-8 shrink-0 items-center justify-center px-3 text-sm opacity-30"
		aria-label="Not available"
	>
		—
	</span>
{/if}
