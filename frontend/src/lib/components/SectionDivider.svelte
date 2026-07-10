<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		label: string;
		icon?: Snippet;
	}

	let { label, icon }: Props = $props();
</script>

<div class="flex items-center gap-2.5 pt-6 pb-3 sm:pt-8 sm:pb-4">
	{#if icon}
		<span class="text-accent">
			{@render icon()}
		</span>
	{/if}
	<span
		class="font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50 whitespace-nowrap select-none"
	>
		{label}
	</span>
	<div class="h-px flex-1 bg-base-content/10"></div>
</div>
