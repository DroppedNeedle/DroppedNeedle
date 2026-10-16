<script lang="ts">
	import { ChevronLeft, ChevronRight } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		current: number;
		total: number;
		onchange: (page: number) => void;
	}

	let { current, total, onchange }: Props = $props();
</script>

{#if total > 1}
	<div class="flex items-center gap-2">
		<IconButton
			variant="neutral"
			size="sm"
			icon={ChevronLeft}
			label="Previous page"
			disabled={current === 1}
			onclick={() => onchange(Math.max(1, current - 1))}
		/>
		<span class="text-sm">{current} / {total}</span>
		<IconButton
			variant="neutral"
			size="sm"
			icon={ChevronRight}
			label="Next page"
			disabled={current === total}
			onclick={() => onchange(Math.min(total, current + 1))}
		/>
	</div>
{/if}
