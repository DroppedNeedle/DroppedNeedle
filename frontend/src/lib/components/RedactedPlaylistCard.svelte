<script lang="ts">
	import type { RedactedPlaylist } from '$lib/api/playlists';
	import { Lock } from 'lucide-svelte';

	let { playlist }: { playlist: RedactedPlaylist } = $props();

	let trackText = $derived(`${playlist.track_count} track${playlist.track_count === 1 ? '' : 's'}`);
</script>

<!-- Admin-only redacted projection of another user's PRIVATE playlist (D4): no name,
	 no cover, no link-through; only count + owner. -->
<div
	class="flex w-full shrink-0 cursor-default select-none flex-col overflow-hidden rounded-2xl border border-dashed border-base-content/12 bg-base-200/30 opacity-80"
	aria-label="Private playlist owned by {playlist.owner_name ?? 'another user'}"
>
	<figure
		class="relative flex aspect-square items-center justify-center overflow-hidden bg-base-content/4"
	>
		<Lock class="h-10 w-10 text-base-content/30" />
	</figure>
	<div class="px-3 pt-3 pb-3">
		<h3 class="line-clamp-1 text-sm font-semibold italic text-base-content/60">Private playlist</h3>
		<p class="mt-0.5 line-clamp-2 text-xs text-base-content/50">
			{trackText}{playlist.owner_name ? ` · owned by ${playlist.owner_name}` : ''}
		</p>
	</div>
</div>
