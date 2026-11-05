<!--
	An artist's Last.fm text in the artist page's About card: the biography
	(it fills the card's height, four lines at least; "Read more" opens the rest), listeners and plays in mono,
	and "View on Last.fm". Its tags join the page's genre chips, so they are
	not repeated here.
-->
<script lang="ts">
	import { ArrowRight, ChevronUp, ExternalLink } from 'lucide-svelte';
	import type { components } from '$lib/api/v3/openapi';
	import { outsideLink } from '$lib/ui/outsideLink';
	import { formatListenCount } from '$lib/utils/formatting';
	import Button from '$lib/ui/Button.svelte';
	import FillText from '$lib/ui/FillText.svelte';

	type LastFmArtistEnrichment = components['schemas']['LastFmArtistEnrichment'];

	interface Props {
		enrichment: LastFmArtistEnrichment | null | undefined;
		loading?: boolean;
	}

	let { enrichment, loading = false }: Props = $props();

	const text = $derived(enrichment?.bio ?? null);
	const link = $derived(outsideLink(enrichment?.url));
	const listeners = $derived(enrichment?.listeners ?? 0);
	const playcount = $derived(enrichment?.playcount ?? 0);

	let expanded = $state(false);
	let clipped = $state(false);
</script>

{#if loading}
	<div class="space-y-2">
		<div class="skeleton h-4 w-full"></div>
		<div class="skeleton h-4 w-full"></div>
		<div class="skeleton h-4 w-3/4"></div>
	</div>
{:else if enrichment}
	<div class="flex min-h-0 flex-col gap-4" class:flex-1={!!text}>
		{#if text}
			<FillText {text} {expanded} bind:clipped />
		{/if}
		{#if listeners > 0 || playcount > 0}
			<p class="font-mono text-xs tracking-[0.12em] text-muted uppercase">
				{#if listeners > 0}<span>{formatListenCount(listeners, true)} listeners</span>{/if}
				{#if listeners > 0 && playcount > 0}<span aria-hidden="true" class="mx-2">·</span>{/if}
				{#if playcount > 0}<span>{formatListenCount(playcount)}</span>{/if}
				<span aria-hidden="true" class="mx-2">·</span><span>Last.fm</span>
			</p>
		{/if}
		{#if (text && (clipped || expanded)) || link}
			<div class="flex flex-wrap items-center gap-2">
				{#if text && expanded}
					<Button
						variant="outline"
						size="sm"
						icon={ChevronUp}
						iconPosition="end"
						onclick={() => (expanded = false)}>Show less</Button
					>
				{:else if text && clipped}
					<Button
						variant="outline"
						size="sm"
						icon={ArrowRight}
						iconPosition="end"
						onclick={() => (expanded = true)}>Read more</Button
					>
				{/if}
				{#if link}
					<Button
						variant="ghost"
						size="sm"
						icon={ExternalLink}
						iconPosition="end"
						href={link}
						target="_blank"
						rel="noopener noreferrer">View on Last.fm</Button
					>
				{/if}
			</div>
		{/if}
	</div>
{/if}
