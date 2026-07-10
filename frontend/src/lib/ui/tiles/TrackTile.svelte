<script lang="ts">
	/*
	 * TrackTile — compact horizontal track tile (recently played etc.).
	 * Preserves: artist link, artwork, listened-at timestamp, and the album
	 * search fallback when the track has no artist MBID.
	 */
	import { goto } from '$app/navigation';
	import { Disc3, Search } from 'lucide-svelte';
	import { artistHrefOrNull } from '$lib/utils/entityRoutes';
	import { formatListenedAt } from '$lib/utils/formatting';

	interface Props {
		name: string;
		artistName?: string | null;
		artistMbid?: string | null;
		albumName?: string | null;
		imageUrl?: string | null;
		listenedAt?: string | null;
	}

	let {
		name,
		artistName = null,
		artistMbid = null,
		albumName = null,
		imageUrl = null,
		listenedAt = null
	}: Props = $props();

	const href = $derived(artistHrefOrNull(artistMbid));

	function searchAlbum(e: MouseEvent): void {
		e.stopPropagation();
		e.preventDefault();
		const query = [artistName, albumName || name].filter(Boolean).join(' ').trim();
		if (query) goto(`/search/albums?q=${encodeURIComponent(query)}`);
	}
</script>

<svelte:element
	this={href ? 'a' : 'div'}
	href={href ?? undefined}
	data-sveltekit-preload-data={href ? 'hover' : undefined}
	class="dn-tracktile group {href ? '' : 'dn-tracktile--static'}"
>
	<figure class="dn-tracktile__art">
		{#if imageUrl}
			<img src={imageUrl} alt={albumName || name} class="h-full w-full object-cover" />
		{:else}
			<div class="flex h-full w-full items-center justify-center bg-base-200">
				<Disc3 class="h-6 w-6 text-base-content/20" />
			</div>
		{/if}
	</figure>
	<div class="min-w-0 flex-1">
		<h3 class="dn-tile__title">{name}</h3>
		{#if artistName}
			<p class="dn-tile__sub">{artistName}</p>
		{/if}
		{#if listenedAt}
			<p class="dn-tracktile__time">{formatListenedAt(listenedAt)}</p>
		{/if}
	</div>
	{#if !artistMbid}
		<button
			type="button"
			class="dn-tile__fallback dn-tile__fallback--inline"
			title="Search for this album"
			onclick={searchAlbum}
		>
			<Search class="h-3.5 w-3.5" />
		</button>
	{/if}
</svelte:element>
