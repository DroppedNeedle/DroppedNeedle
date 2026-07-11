<script lang="ts">
	import { resolve } from '$app/paths';
	import { Music, ArrowLeft } from 'lucide-svelte';
	import { page } from '$app/state';
</script>

<svelte:head>
	<title>Playlist Error - DroppedNeedle</title>
</svelte:head>

<div class="w-full px-2 sm:px-4 lg:px-8 py-4 sm:py-8 max-w-7xl mx-auto">
	<div class="flex flex-col items-center justify-center py-20 gap-4 text-center">
		<Music class="h-16 w-16 text-base-content/20" />
		<h1 class="text-lg font-semibold text-base-content/80">Couldn't load this playlist</h1>
		<p class="text-sm text-base-content/60">
			{page.error?.message ?? 'Please try again'}
			{#if page.error && 'status' in page.error && page.error.status}
				({page.error.status})
			{/if}
		</p>
		<div class="flex items-center gap-2">
			<a href={resolve('/playlists')} class="btn btn-ghost btn-sm">
				<ArrowLeft class="h-4 w-4" />
				Back to Playlists
			</a>
			<button class="btn btn-accent btn-sm" onclick={() => location.reload()}> Retry </button>
		</div>
	</div>
</div>
