<script lang="ts">
	import { YOUTUBE_PLAYER_ELEMENT_ID } from '$lib/constants';

	interface Props {
		elementId?: string;
	}

	let { elementId = YOUTUBE_PLAYER_ELEMENT_ID }: Props = $props();
</script>

<!--
	YouTube's Developer Policies demand a player viewport of at least 200x200 and forbid hiding
	or obscuring it: no `hidden` wrapper, no user-collapsible panel, nothing below the inline
	floor. z-[60] keeps a transient toast (z-50) off it.
-->
<div
	data-testid="youtube-player"
	class="droppedneedle-yt-popout fixed right-3 z-[60] overflow-hidden rounded-lg shadow-lg ring-1 ring-base-content/10 sm:right-4"
	style:min-width="200px"
	style:min-height="200px"
>
	<div id={elementId} data-testid="youtube-player-mount" class="h-full w-full"></div>
</div>
