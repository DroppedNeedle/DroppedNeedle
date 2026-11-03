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
	<!-- YouTube's widget copies this element's attributes onto its iframe: never give it a style. -->
	<div id={elementId} data-testid="youtube-player-mount" class="h-full w-full"></div>
</div>

<style>
	/* Pops out above the bar so the bar keeps its height. YouTube's Developer Policies demand a
	   200x200 viewport and forbid hiding the player, so never collapse or breakpoint-hide this.
	   The floor is repeated inline on the element, so it survives this stylesheet changing. */
	.droppedneedle-yt-popout {
		bottom: calc(var(--ms-bottom-nav-offset) + var(--ms-player-height) + 0.75rem);
		width: clamp(200px, calc(100vw - 1.5rem), 356px);
		height: 200px;
		animation: droppedneedle-yt-popout-in 200ms ease-out;
	}

	/* YT.Player replaces the mount div with an iframe carrying its own width/height attributes.
	   Force it to fill the compliant box regardless of what those attributes say. */
	.droppedneedle-yt-popout > :global(iframe) {
		display: block;
		width: 100%;
		height: 100%;
	}

	@keyframes droppedneedle-yt-popout-in {
		from {
			opacity: 0;
			transform: translateY(0.5rem) scale(0.96);
		}
		to {
			opacity: 1;
			transform: none;
		}
	}
</style>
