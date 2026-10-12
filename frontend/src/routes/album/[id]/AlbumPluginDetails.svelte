<script lang="ts">
	import { ExternalLink, Puzzle } from 'lucide-svelte';
	import { outsideLink } from '$lib/components/outsideLink';
	import { getAlbumPluginDetailsQuery } from '$lib/queries/albums/GetItQueries.svelte';

	interface Props {
		releaseGroupMbid: string;
		/** the page already shows notes from a first-party source */
		hasNotes?: boolean;
		/** tags the page already shows */
		knownTags?: string[];
	}
	let { releaseGroupMbid, hasNotes = false, knownTags = [] }: Props = $props();

	const detailsQuery = getAlbumPluginDetailsQuery(() => releaseGroupMbid);
	const details = $derived(detailsQuery.data);

	// Plugins only fill gaps: their notes show when nothing else gave any,
	// and only tags the page does not show yet.
	const notes = $derived(hasNotes ? null : (details?.biography ?? null));
	const tags = $derived.by(() => {
		const known = new Set(knownTags.map((tag) => tag.toLowerCase()));
		return (details?.tags ?? []).filter((tag) => !known.has(tag.toLowerCase()));
	});
	const links = $derived((details?.links ?? []).filter((link) => outsideLink(link)));
	const hasContent = $derived(!!notes || tags.length > 0 || links.length > 0);

	function hostOf(url: string): string {
		try {
			return new URL(url).hostname.replace(/^www\./, '');
		} catch {
			return url;
		}
	}
</script>

{#if hasContent}
	<section class="bg-base-200/50 rounded-box p-4 sm:p-6 space-y-3" aria-label="From plugins">
		<div class="flex items-center gap-2">
			<span class="badge badge-sm badge-ghost gap-1">
				<Puzzle class="h-3 w-3" />
				From plugins
			</span>
		</div>
		{#if notes}
			<p class="text-sm leading-relaxed whitespace-pre-line opacity-80">{notes}</p>
		{/if}
		{#if tags.length > 0}
			<div class="flex flex-wrap gap-2">
				{#each tags as tag (tag)}
					<span class="badge badge-outline badge-sm">{tag}</span>
				{/each}
			</div>
		{/if}
		{#if links.length > 0}
			<ul class="flex flex-wrap gap-2">
				{#each links as link (link)}
					<li>
						<a
							href={link}
							target="_blank"
							rel="noopener noreferrer"
							class="btn btn-ghost btn-xs gap-1 min-h-11 sm:min-h-0"
						>
							<ExternalLink class="h-3 w-3" />
							{hostOf(link)}
						</a>
					</li>
				{/each}
			</ul>
		{/if}
	</section>
{/if}
