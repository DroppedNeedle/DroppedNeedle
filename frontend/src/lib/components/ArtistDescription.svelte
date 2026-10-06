<script lang="ts">
	import { ChevronUp, ChevronDown } from 'lucide-svelte';
	import { colors } from '$lib/colors';

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

<div class="bg-base-200/50 rounded-box p-4 sm:p-6">
	{#if loading}
		<div class="space-y-2" data-testid="artist-description-skeleton">
			<div class="skeleton h-4 w-full"></div>
			<div class="skeleton h-4 w-full"></div>
			<div class="skeleton h-4 w-3/4"></div>
		</div>
	{:else if description}
		<div class="text-sm sm:text-base text-base-content/80 leading-relaxed">
			{#if descriptionExpanded}
				<div>
					<!-- eslint-disable-next-line svelte/no-at-html-tags -->
					{@html description.replace(/\n\n/g, '<br><br>').replace(/\n/g, '<br>')}
				</div>
				<button
					class="btn btn-sm mt-3 gap-2"
					style="background-color: {colors.accent}; color: {colors.secondary};"
					onclick={() => (descriptionExpanded = false)}
				>
					Show Less
					<ChevronUp class="h-4 w-4" />
				</button>
			{:else}
				<div
					bind:this={descriptionElement}
					class="line-clamp-4 overflow-hidden"
					style="display: -webkit-box; -webkit-box-orient: vertical;"
				>
					<!-- eslint-disable-next-line svelte/no-at-html-tags -->
					{@html description.replace(/\n\n/g, '<br><br>').replace(/\n/g, '<br>')}
				</div>
				{#if showViewMore}
					<button
						class="btn btn-sm mt-3 gap-2"
						style="background-color: {colors.accent}; color: {colors.secondary};"
						onclick={() => (descriptionExpanded = true)}
					>
						Read More
						<ChevronDown class="h-4 w-4" />
					</button>
				{/if}
			{/if}
		</div>
	{:else}
		<p class="text-base-content/50 italic text-sm">No biography available</p>
	{/if}
</div>
