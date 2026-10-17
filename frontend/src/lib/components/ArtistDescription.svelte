<script lang="ts">
	import { ChevronUp, ChevronDown } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		description: string | null | undefined;
		loading?: boolean;
	}

	let { description, loading = false }: Props = $props();

	let descriptionExpanded = $state(false);
	let descriptionElement = $state<HTMLElement | null>(null);
	let showViewMore = $state(false);

	function checkDescriptionHeight() {
		if (descriptionElement && !descriptionExpanded) {
			const lineHeight = parseFloat(getComputedStyle(descriptionElement).lineHeight);
			const actualHeight = descriptionElement.scrollHeight;
			const fourLines = lineHeight * 4;
			showViewMore = actualHeight > fourLines;
		}
	}

	// Measure once the text has rendered; a new description measures again.
	$effect(() => {
		if (!description || loading) return;
		const timer = setTimeout(() => checkDescriptionHeight(), 50);
		return () => clearTimeout(timer);
	});
</script>

<Card>
	{#if loading}
		<div class="space-y-2" data-testid="artist-description-skeleton">
			<div class="skeleton h-4 w-full"></div>
			<div class="skeleton h-4 w-full"></div>
			<div class="skeleton h-4 w-3/4"></div>
		</div>
	{:else if description}
		<div class="text-sm sm:text-base text-base-content/80 leading-relaxed">
			{#if descriptionExpanded}
				<div class="whitespace-pre-line">{description}</div>
				<Button
					variant="accent"
					size="sm"
					icon={ChevronUp}
					iconPosition="end"
					class="mt-3"
					onclick={() => (descriptionExpanded = false)}
				>
					Show Less
				</Button>
			{:else}
				<div
					bind:this={descriptionElement}
					class="line-clamp-4 overflow-hidden whitespace-pre-line"
				>
					{description}
				</div>
				{#if showViewMore}
					<Button
						variant="accent"
						size="sm"
						icon={ChevronDown}
						iconPosition="end"
						class="mt-3"
						onclick={() => (descriptionExpanded = true)}
					>
						Read More
					</Button>
				{/if}
			{/if}
		</div>
	{:else}
		<p class="text-base-content/50 italic text-sm">No biography available</p>
	{/if}
</Card>
