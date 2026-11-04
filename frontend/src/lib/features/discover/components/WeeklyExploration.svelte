<script lang="ts">
	import type { WeeklyExploration as SectionType } from '$lib/features/discover/queries/types';
	import { CalendarDays, ExternalLink } from 'lucide-svelte';
	import HorizontalCarousel from '$lib/ui/HorizontalCarousel.svelte';
	import WeeklyExplorationCard from './WeeklyExplorationCard.svelte';
	import LiveUpdatingBadge from '$lib/ui/LiveUpdatingBadge.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { outsideLink } from '$lib/ui/outsideLink';

	interface Props {
		section: SectionType;
		updating?: boolean;
	}

	let { section, updating = false }: Props = $props();
	const sourceLink = $derived(outsideLink(section.source_url));

	function formatPlaylistDate(dateStr: string): string {
		try {
			const d = new Date(dateStr);
			return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
		} catch {
			return '';
		}
	}

	const formattedDate = $derived(formatPlaylistDate(section.playlist_date));
</script>

<section class="mb-6 sm:mb-8">
	<SectionHead title="Weekly Exploration">
		{#snippet chips()}
			{#if formattedDate}
				<StatusChip tone="action" size="xs" icon={CalendarDays}>{formattedDate}</StatusChip>
			{/if}
			{#if updating}
				<LiveUpdatingBadge label="Updating weekly picks" className="px-2 py-0.5" />
			{/if}
		{/snippet}
		{#snippet actions()}
			{#if sourceLink}
				<a
					href={sourceLink}
					target="_blank"
					rel="noopener noreferrer"
					class="weekly-source"
					title="View on ListenBrainz"
				>
					ListenBrainz
					<ExternalLink class="size-3.5" aria-hidden="true" />
				</a>
			{/if}
		{/snippet}
	</SectionHead>

	<HorizontalCarousel class="-mx-4 px-4 pb-2 sm:mx-0 sm:px-0">
		{#each section.tracks as track (track.artist_name + track.title)}
			<WeeklyExplorationCard {track} />
		{/each}
	</HorizontalCarousel>
</section>

<style>
	.weekly-source {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		min-height: 2rem;
		padding-inline: 0.25rem;
		border-radius: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		white-space: nowrap;
		transition: color 150ms;
	}
	.weekly-source:hover {
		color: var(--dn-title);
	}
</style>
