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
	<!-- Each source keeps its own colour, which the kit's variants do not cover.
	     The icon takes the theme's page colour: the source hues are tuned
	     against it, so it reads on them on light and dark paper alike. -->
	<Button
		size="xs"
		icon={Play}
		class="shrink-0 [&_svg]:fill-current"
		style="--btn-color: {sourceColor}; background-color: var(--btn-color); color: var(--color-base-100);"
		{onclick}
		aria-label={ariaLabel}
	>
		{@render icon()}
	</Button>
{:else}
	<span
		class="inline-flex h-6 min-w-8 shrink-0 items-center justify-center px-2 text-sm opacity-40"
		aria-label="Not available"
	>
		—
	</span>
{/if}
