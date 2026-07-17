<script lang="ts">
	import type { Snippet } from 'svelte';
	import LiveUpdatingBadge from './LiveUpdatingBadge.svelte';

	interface Props {
		label: string;
		icon?: Snippet;
		updating?: boolean;
	}

	let { label, icon, updating = false }: Props = $props();
</script>

<div class="flex items-center gap-3 pt-6 pb-2 sm:pt-8 sm:pb-3">
	{#if icon}
		<span class="text-base-content/30">
			{@render icon()}
		</span>
	{/if}
	<span
		class="font-display text-[12px] font-semibold uppercase tracking-[0.18em] text-base-content/40 whitespace-nowrap select-none"
	>
		{label}
	</span>
	{#if updating}
		<LiveUpdatingBadge label="Updating" className="px-2 py-0.5 text-[11px]" />
	{/if}
	<div class="h-px flex-1 bg-gradient-to-r from-base-content/8 to-transparent"></div>
</div>
