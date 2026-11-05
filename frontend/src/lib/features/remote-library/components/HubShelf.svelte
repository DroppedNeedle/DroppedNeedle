<!--
	One hub shelf: the Anton heading (a link with "View all" when the shelf
	has a page), then its content in the bordered panel, or bare for a row of
	covers.
-->
<script lang="ts">
	import './hub.css';
	import CarouselSkeleton from '$lib/ui/CarouselSkeleton.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import type { KitIcon } from '$lib/ui/Button.svelte';
	import type { Snippet } from 'svelte';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		title: string;
		icon?: KitIcon;
		seeAllHref?: string;
		loading?: boolean;
		/** No panel: a row of covers sits on the page. */
		bare?: boolean;
		/** A mono line beside the title. */
		sub?: string;
		actions?: Snippet;
		children: Snippet;
	}

	let {
		title,
		icon,
		seeAllHref,
		loading = false,
		bare = false,
		sub,
		actions,
		children
	}: Props = $props();
</script>

<section class="hub-shelf">
	<SectionHead
		{title}
		{icon}
		href={seeAllHref ? withBasePath(seeAllHref) : null}
		moreLabel="View all"
		{actions}
	>
		{#snippet chips()}
			{#if sub}<p class="hub-sub">{sub}</p>{/if}
		{/snippet}
	</SectionHead>

	<div class={bare ? '' : 'hub-panel'}>
		{#if loading}
			<CarouselSkeleton />
		{:else}
			{@render children()}
		{/if}
	</div>
</section>

<style>
	.hub-shelf {
		display: flex;
		min-width: 0;
		flex-direction: column;
	}
	.hub-shelf > div {
		flex: 1;
	}
</style>
