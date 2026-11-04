<!--
	The fallback for cover or artist art that is missing or still loading: a
	full-strength gradient with a mark on top, a record for an album and the
	name's first letter for an artist (a person icon when there is no name).
	The colours come from the id, or from the name when there is no valid id,
	so an item looks the same on every screen. Fills its nearest positioned
	ancestor, so the parent must be `relative`. `className` goes on the root,
	for example to fade the placeholder out once the image is in.
-->
<script lang="ts">
	import { Users } from 'lucide-svelte';
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
	const initial = $derived(name.trim().charAt(0).toLocaleUpperCase());
</script>

<div
	class="pointer-events-none absolute inset-0 flex items-center justify-center transition-opacity duration-300 @container {look.ink} {className}"
	style:background-image={look.background}
	aria-hidden="true"
	data-testid="cover-placeholder"
>
	{#if kind === 'album'}
		<svg viewBox="0 0 100 100" class="h-1/2 w-1/2 opacity-80" fill="none" stroke="currentColor">
			<circle cx="50" cy="50" r="46" stroke-width="5" />
			<circle cx="50" cy="50" r="36" stroke-width="1.5" opacity="0.6" />
			<circle cx="50" cy="50" r="29" stroke-width="1.5" opacity="0.6" />
			<circle cx="50" cy="50" r="10" stroke-width="10" />
		</svg>
	{:else if initial}
		<span class="font-display text-[40cqw] leading-none uppercase opacity-80">{initial}</span>
	{:else}
		<Users class="h-1/3 w-1/3" />
	{/if}
</div>
