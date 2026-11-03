<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import { ChevronLeft, ChevronRight } from 'lucide-svelte';

	interface Props {
		/** Zero-based. */
		page: number;
		pages: number;
		ongo: (page: number) => void;
	}

	let { page, pages, ongo }: Props = $props();
</script>

{#if pages > 1}
	<div class="mt-8 flex items-center justify-center gap-2">
		<Button
			variant="ghost"
			size="sm"
			icon={ChevronLeft}
			disabled={page === 0}
			onclick={() => ongo(page - 1)}
			aria-label="Previous page"
		/>
		<span class="text-sm text-base-content/70">
			Page {page + 1} of {pages}
		</span>
		<Button
			variant="ghost"
			size="sm"
			icon={ChevronRight}
			disabled={page >= pages - 1}
			onclick={() => ongo(page + 1)}
			aria-label="Next page"
		/>
	</div>
{/if}
