<script lang="ts">
	import {
		ChevronDown,
		ChevronUp,
		Disc3,
		Download,
		Gift,
		Search,
		ShoppingBag
	} from 'lucide-svelte';
	import { getPurchaseOptionsQuery } from '$lib/queries/albums/GetItQueries.svelte';
	import type { PurchaseLink } from '$lib/types';
	import { outsideLink } from '$lib/components/outsideLink';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		releaseGroupMbid: string;
		enabled?: boolean;
	}
	let { releaseGroupMbid, enabled = true }: Props = $props();

	const optionsQuery = getPurchaseOptionsQuery(
		() => releaseGroupMbid,
		() => enabled
	);
	const options = $derived(optionsQuery.data);

	let expanded = $state(false);

	const PREVIEW_LIMIT = 5;

	const linkable = (links: PurchaseLink[]) => links.filter((link) => outsideLink(link.url));
	const groups = $derived(
		options
			? ([
					{ title: 'Digital', icon: Download, links: linkable(options.digital) },
					{ title: 'Vinyl & CD', icon: Disc3, links: linkable(options.physical) },
					{ title: 'Free downloads', icon: Gift, links: linkable(options.free) }
				] as { title: string; icon: typeof Download; links: PurchaseLink[] }[])
			: []
	);
	const allLinks = $derived(groups.flatMap((group) => group.links));
	const hasDirectLinks = $derived(allLinks.length > 0);

	// Collapsed row: every kind is eligible (an album can have no digital links
	// at all - vinyl-only reissues), in fairness order, at most one pill per
	// store so a release with a dozen Amazon ASINs doesn't fill the row.
	const preview = $derived.by(() => {
		const seen: Record<string, true> = {};
		const picked: PurchaseLink[] = [];
		for (const link of allLinks) {
			const key = `${link.store}:${link.kind}`;
			if (seen[key]) continue;
			seen[key] = true;
			picked.push(link);
			if (picked.length === PREVIEW_LIMIT) break;
		}
		return picked;
	});
	const hiddenCount = $derived(Math.max(0, allLinks.length - preview.length));
</script>

{#snippet storeLink(link: PurchaseLink, withKindIcon = false)}
	<Button
		size="xs"
		pill
		href={link.url}
		target="_blank"
		rel="noopener noreferrer"
		title={link.kind === 'physical'
			? `${link.label} - vinyl or CD`
			: link.kind === 'free'
				? `${link.label} - free download`
				: link.label}
	>
		{#if withKindIcon && link.kind === 'physical'}
			<Disc3 class="h-3 w-3 opacity-60" aria-hidden="true" />
		{:else if withKindIcon && link.kind === 'free'}
			<Gift class="h-3 w-3 opacity-60" aria-hidden="true" />
		{/if}
		{link.label}
	</Button>
{/snippet}

<!-- Always renders: worst case it offers the Bandcamp search.
     One line collapsed, above the track list; expand for every store + groups. -->
<Card as="section" padding="sm" aria-label="Where to buy">
	<div class="flex flex-wrap items-center gap-2">
		<div class="flex items-center gap-1.5">
			<ShoppingBag class="h-4 w-4 text-primary" aria-hidden="true" />
			<h2 class="text-sm font-semibold">Where to buy</h2>
		</div>

		{#if optionsQuery.isLoading}
			<div class="skeleton h-6 w-24 rounded-full"></div>
			<div class="skeleton h-6 w-20 rounded-full"></div>
		{:else}
			{#each preview as link (link.url)}
				{@render storeLink(link, true)}
			{/each}
			{#if !hasDirectLinks && options?.bandcamp_search_url}
				<Button
					size="xs"
					pill
					icon={Search}
					href={options.bandcamp_search_url}
					target="_blank"
					rel="noopener noreferrer"
				>
					Search Bandcamp
				</Button>
			{/if}
			{#if hasDirectLinks}
				<Button
					variant="ghost"
					size="xs"
					icon={expanded ? ChevronUp : ChevronDown}
					onclick={() => (expanded = !expanded)}
					aria-expanded={expanded}
				>
					{expanded ? 'Less' : hiddenCount > 0 ? `${hiddenCount} more` : 'Details'}
				</Button>
			{/if}
		{/if}
	</div>

	{#if expanded && options}
		<div class="mt-3 space-y-3 border-t border-base-content/10 pt-3">
			{#each groups as group (group.title)}
				{#if group.links.length}
					{@const GroupIcon = group.icon}
					<div>
						<h3
							class="mb-1.5 flex items-center gap-1.5 text-[11px] font-semibold tracking-wide text-base-content/50 uppercase"
						>
							<GroupIcon class="h-3.5 w-3.5" aria-hidden="true" />
							{group.title}
						</h3>
						<div class="flex flex-wrap gap-2">
							{#each group.links as link (link.url)}
								{@render storeLink(link)}
							{/each}
						</div>
					</div>
				{/if}
			{/each}

			{#if options.bandcamp_search_url}
				<Button
					size="xs"
					pill
					icon={Search}
					href={options.bandcamp_search_url}
					target="_blank"
					rel="noopener noreferrer"
				>
					Search Bandcamp
				</Button>
			{/if}

			<p class="text-[11px] text-base-content/40">
				Buy it once, own it forever - your purchase supports the artist.
			</p>
		</div>
	{/if}
</Card>
