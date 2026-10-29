<!--
	The caller's heart on a library album, track or artist: filled when it
	is one of their favourites. Sits beside RatingStars, at the same size.
-->
<script lang="ts">
	import { Heart } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		favorite: boolean;
		/** The new state: true adds it to the favourites. */
		onchange: (favorite: boolean) => void;
		/** 'sm' on rows, 'md' in headers, as RatingStars. */
		size?: 'sm' | 'md';
		class?: string;
	}

	let { favorite, onchange, size = 'sm', class: className = '' }: Props = $props();
</script>

<IconButton
	icon={Heart}
	shape="square"
	size={size === 'md' ? 'sm' : 'xs'}
	variant={favorite ? 'danger-ghost' : 'ghost'}
	label={favorite ? 'Remove from favourites' : 'Add to favourites'}
	class="{favorite ? '[&_svg]:fill-current' : ''} {size === 'md'
		? '[&_svg]:size-5'
		: ''} {className}"
	onclick={() => onchange(!favorite)}
/>
