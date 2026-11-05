<!--
	The MusicBrainz biography in the About card: four lines, then "Read more"
	opens the rest in place.
-->
<script lang="ts">
	import { ArrowRight, ChevronUp } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		description: string | null | undefined;
		loading?: boolean;
	}

	let { description, loading = false }: Props = $props();

	let expanded = $state(false);
	let element = $state<HTMLElement | null>(null);
	let clipped = $state(false);

	// Measure once the text has rendered; a new description measures again.
	$effect(() => {
		if (!description || loading) return;
		const timer = setTimeout(() => {
			if (!element || expanded) return;
			const lineHeight = parseFloat(getComputedStyle(element).lineHeight);
			clipped = element.scrollHeight > lineHeight * 4;
		}, 50);
		return () => clearTimeout(timer);
	});
</script>

{#if loading}
	<div class="space-y-2" data-testid="artist-description-skeleton">
		<div class="skeleton h-4 w-full"></div>
		<div class="skeleton h-4 w-full"></div>
		<div class="skeleton h-4 w-3/4"></div>
	</div>
{:else if description}
	<div class="leading-relaxed">
		<div bind:this={element} class="whitespace-pre-line" class:line-clamp-4={!expanded}>
			{description}
		</div>
		{#if expanded}
			<Button
				variant="outline"
				size="sm"
				icon={ChevronUp}
				iconPosition="end"
				class="mt-4"
				onclick={() => (expanded = false)}
			>
				Show less
			</Button>
		{:else if clipped}
			<Button
				variant="outline"
				size="sm"
				icon={ArrowRight}
				iconPosition="end"
				class="mt-4"
				onclick={() => (expanded = true)}
			>
				Read more
			</Button>
		{/if}
	</div>
{:else}
	<p class="text-sm text-muted italic">No biography available</p>
{/if}
