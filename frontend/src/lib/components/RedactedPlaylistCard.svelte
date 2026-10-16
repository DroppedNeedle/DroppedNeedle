<script lang="ts">
	import type { RedactedPlaylist } from '$lib/api/playlists';
	import { Lock } from 'lucide-svelte';

	let { playlist }: { playlist: RedactedPlaylist } = $props();

	let trackText = $derived(`${playlist.track_count} track${playlist.track_count === 1 ? '' : 's'}`);
</script>

<!-- Admin-only redacted projection of another user's PRIVATE playlist: no name,
	 no cover, no link-through; only count + owner. -->
<div
	class="card card-sm w-full shrink-0 bg-base-200/40 opacity-70 select-none cursor-default"
	aria-label="Private playlist owned by {playlist.owner_name ?? 'another user'}"
>
	<figure
		class="aspect-square overflow-hidden relative flex items-center justify-center rounded-t-box bg-base-300/40"
	>
		<Lock class="h-10 w-10 text-base-content/30" />
	</figure>
	<div class="px-3 pt-3 pb-3">
		<h3 class="text-sm font-semibold italic text-base-content/60 line-clamp-1">Private playlist</h3>
		<p class="text-xs text-base-content/50 mt-0.5 line-clamp-2">
			{trackText}{playlist.owner_name ? ` · owned by ${playlist.owner_name}` : ''}
		</p>
	</div>
</div>
