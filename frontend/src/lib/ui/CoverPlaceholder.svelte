<!--
	The fallback for cover or artist art that is missing or still loading: a
	full-strength gradient with a disc (album) or person (artist) icon on top.
	The colours come from the id, or from the name when there is no valid id,
	so an item looks the same on every screen. Fills its nearest positioned
	ancestor, so the parent must be `relative`. `className` goes on the root,
	for example to fade the placeholder out once the image is in.
-->
<script lang="ts">
	import { Disc3, Users } from 'lucide-svelte';
	import { isValidMbid } from '$lib/utils/formatting';
	import { coverPlaceholder } from './coverPlaceholder';

	interface Props {
		/** MusicBrainz id the colours are picked from. */
		id?: string | null;
		/** Title or name; the colours come from it when there is no valid id. */
		name?: string;
		kind?: 'album' | 'artist';
		className?: string;
	}

	let { id = null, name = '', kind = 'album', className = '' }: Props = $props();

	let look = $derived(coverPlaceholder(id && isValidMbid(id) ? id : name));
</script>

<div
	class="pointer-events-none absolute inset-0 flex items-center justify-center transition-opacity duration-300 {look.ink} {className}"
	style:background-image={look.background}
	aria-hidden="true"
	data-testid="cover-placeholder"
>
	{#if kind === 'album'}
		<Disc3 class="h-1/3 w-1/3" />
	{:else}
		<Users class="h-1/3 w-1/3" />
	{/if}
</div>
