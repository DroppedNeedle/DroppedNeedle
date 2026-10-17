<script lang="ts">
	import { Music, ArrowLeft } from 'lucide-svelte';
	import { page } from '$app/state';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
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
			<Button variant="ghost" size="sm" icon={ArrowLeft} href={withBasePath('/playlists')}>
				Back to Playlists
			</Button>
			<Button variant="accent" size="sm" onclick={() => location.reload()}>Retry</Button>
		</div>
	</div>
</div>
