<!--
	The MusicBrainz biography in the About card: it fills the card's height
	(four lines at least), then "Read more" opens the rest in place.
-->
<script lang="ts">
	import { ArrowRight, ChevronUp } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import FillText from '$lib/ui/FillText.svelte';

	interface Props {
		description: string | null | undefined;
		loading?: boolean;
	}

	let { description, loading = false }: Props = $props();

	let expanded = $state(false);
	let clipped = $state(false);
</script>

{#if loading}
	<div class="space-y-2" data-testid="artist-description-skeleton">
		<div class="skeleton h-4 w-full"></div>
		<div class="skeleton h-4 w-full"></div>
		<div class="skeleton h-4 w-3/4"></div>
	</div>
{:else if description}
	<div class="flex min-h-0 flex-1 flex-col">
		<FillText text={description} {expanded} bind:clipped class="whitespace-pre-line" />
		{#if expanded}
			<Button
				variant="outline"
				size="sm"
				icon={ChevronUp}
				iconPosition="end"
				class="mt-4 self-start"
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
				class="mt-4 self-start"
				onclick={() => (expanded = true)}
			>
				Read more
			</Button>
		{/if}
	</div>
{:else}
	<p class="text-sm text-muted italic">No biography available</p>
{/if}
