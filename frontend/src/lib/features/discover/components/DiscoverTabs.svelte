<!--
	The strip of Discover's pages (`DISCOVER_PAGES`), the same under the
	title on Discover and on each of its pages, the one shown filled. Later
	always shows its count, 0 included.
-->
<script lang="ts">
	import PageTabs, { type PageTab } from '$lib/ui/PageTabs.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { getLaterPileQuery } from '$lib/features/discover/queries/later.svelte';
	import { DISCOVER_PAGES, type DiscoverPageId } from '$lib/features/discover/state/discoverPages';

	interface Props {
		current: DiscoverPageId;
		/** Layout only: margin. */
		class?: string;
	}

	let { current, class: className = '' }: Props = $props();

	const laterQuery = getLaterPileQuery();
	const laterCount = $derived(laterQuery.data?.total ?? laterQuery.data?.items.length ?? 0);

	const tabs = $derived<PageTab[]>(
		DISCOVER_PAGES.map((page) => ({
			id: page.id,
			label: page.label,
			href: withBasePath(page.path),
			icon: page.icon,
			count: page.id === 'later' ? laterCount : null
		}))
	);
</script>

<PageTabs {tabs} {current} label="Discover pages" class={className} />
