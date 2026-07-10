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

	interface Props {
		releaseGroupMbid: string;
	}
	let { releaseGroupMbid }: Props = $props();

	const optionsQuery = getPurchaseOptionsQuery(() => releaseGroupMbid);
	const options = $derived(optionsQuery.data);

	let expanded = $state(false);

	const PREVIEW_LIMIT = 5;

	const groups = $derived(
		options
			? ([
					{ title: 'Digital', icon: Download, links: options.digital },
					{ title: 'Vinyl & CD', icon: Disc3, links: options.physical },
					{ title: 'Free downloads', icon: Gift, links: options.free }
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
	<a
		href={link.url}
		target="_blank"
		rel="noopener noreferrer"
		class="btn btn-xs gap-1 rounded-full border-base-content/15 bg-base-200 hover:border-primary hover:bg-base-100"
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
	</a>
{/snippet}

<!-- Always renders (owner-signed): worst case it offers the Bandcamp search.
     One line collapsed, above the track list; expand for every store + groups. -->
<section aria-labelledby="where-to-buy-heading" class="rounded-2xl bg-base-200/40 px-3 py-2">
	<div class="flex flex-wrap items-center gap-2">
		<div class="flex items-center gap-1.5">
			<ShoppingBag class="h-4 w-4 text-primary" aria-hidden="true" />
			<h2 id="where-to-buy-heading" class="text-sm font-semibold">Where to buy</h2>
		</div>

		{#if optionsQuery.isLoading}
			<div class="skeleton h-6 w-24 rounded-full"></div>
			<div class="skeleton h-6 w-20 rounded-full"></div>
		{:else}
			{#each preview as link (link.url)}
				{@render storeLink(link, true)}
			{/each}
			{#if !hasDirectLinks && options?.bandcamp_search_url}
				<a
					href={options.bandcamp_search_url}
					target="_blank"
					rel="noopener noreferrer"
					class="btn btn-xs rounded-full border-dashed border-base-content/25 bg-transparent hover:border-primary"
				>
					<Search class="h-3 w-3" aria-hidden="true" />
					Search Bandcamp
				</a>
			{/if}
			{#if hasDirectLinks}
				<button
					class="btn btn-ghost btn-xs gap-1 text-base-content/60"
					onclick={() => (expanded = !expanded)}
					aria-expanded={expanded}
				>
					{#if expanded}
						<ChevronUp class="h-3.5 w-3.5" aria-hidden="true" /> Less
					{:else}
						<ChevronDown class="h-3.5 w-3.5" aria-hidden="true" />
						{hiddenCount > 0 ? `${hiddenCount} more` : 'Details'}
					{/if}
				</button>
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
				<a
					href={options.bandcamp_search_url}
					target="_blank"
					rel="noopener noreferrer"
					class="btn btn-xs rounded-full border-dashed border-base-content/25 bg-transparent hover:border-primary"
				>
					<Search class="h-3 w-3" aria-hidden="true" />
					Search Bandcamp
				</a>
			{/if}

			<p class="text-[11px] text-base-content/40">
				Buy it once, own it forever - your purchase supports the artist.
			</p>
			{#if options.disclosure}
				<p class="text-[11px] text-base-content/40">
					Some links earn DroppedNeedle a commission at no cost to you.
				</p>
			{/if}
		</div>
	{/if}
</section>
