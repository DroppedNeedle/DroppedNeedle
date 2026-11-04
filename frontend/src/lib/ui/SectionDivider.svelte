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
		<span class="text-muted" aria-hidden="true">
			{@render icon()}
		</span>
	{/if}
	<span
		class="font-mono text-xs font-semibold uppercase tracking-[0.16em] text-muted whitespace-nowrap select-none"
	>
		{label}
	</span>
	{#if updating}
		<LiveUpdatingBadge label="Updating" className="px-2 py-0.5 text-[11px]" />
	{/if}
	<div class="h-px flex-1 bg-line" aria-hidden="true"></div>
</div>
