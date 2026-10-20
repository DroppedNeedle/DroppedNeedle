<script lang="ts">
	import { ChevronLeft, ChevronRight } from 'lucide-svelte';
	import type { Snippet } from 'svelte';

	interface Props {
		children: Snippet;
		class?: string;
		onNearEnd?: () => void;
	}

	let { children, class: className = '', onNearEnd }: Props = $props();

	let container: HTMLDivElement | undefined = $state();
	let showLeftArrow = $state(false);
	let showRightArrow = $state(true);

	function updateArrows() {
		if (!container) return;
		const { scrollLeft, scrollWidth, clientWidth } = container;
		showLeftArrow = scrollLeft > 10;
		showRightArrow = scrollLeft < scrollWidth - clientWidth - 10;

		if (onNearEnd) {
			const scrollPercentage = (scrollLeft + clientWidth) / scrollWidth;
			if (scrollPercentage > 0.8) onNearEnd();
		}
	}

	function scrollLeft() {
		container?.scrollBy({ left: -container.clientWidth * 0.8, behavior: 'smooth' });
	}

	function scrollRight() {
		container?.scrollBy({ left: container.clientWidth * 0.8, behavior: 'smooth' });
	}

	$effect(() => {
		if (!container) return;

		const observer = new ResizeObserver(() => {
			updateArrows();
		});
		observer.observe(container);

		updateArrows();

		return () => {
			observer.disconnect();
		};
	});
	import IconButton from '$lib/ui/IconButton.svelte';
</script>

<div class="relative group/carousel">
	{#if showLeftArrow}
		<IconButton
			variant="neutral"
			size="sm"
			icon={ChevronLeft}
			label="Scroll left"
			class="absolute left-0 top-1/2 z-10 hidden animate-slide-in-left sm:flex"
			onclick={scrollLeft}
		/>
	{/if}

	<div
		bind:this={container}
		data-lazy-strip
		onscroll={updateArrows}
		class="flex gap-4 overflow-x-auto overflow-y-hidden pb-8 -mb-8 scrollbar-hide snap-x snap-mandatory scroll-pl-4 [&>*]:snap-start {className}"
	>
		{@render children()}
	</div>

	{#if showRightArrow}
		<IconButton
			variant="neutral"
			size="sm"
			icon={ChevronRight}
			label="Scroll right"
			class="absolute right-0 top-1/2 z-10 hidden animate-slide-in-right sm:flex"
			onclick={scrollRight}
		/>
	{/if}
</div>
