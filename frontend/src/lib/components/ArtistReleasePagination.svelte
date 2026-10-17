<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		loadedCount: number;
		totalCount?: number | null;
		loading?: boolean;
		onloadmore: () => void;
	}

	let { loadedCount, totalCount = null, loading = false, onloadmore }: Props = $props();
</script>

<Card variant="bordered" padding="md">
	<div class="flex flex-col items-center justify-between gap-3 sm:flex-row">
		<p id="artist-release-progress" class="text-sm text-base-content/60" aria-live="polite">
			{#if totalCount}
				{loadedCount} of {totalCount} releases loaded
			{:else}
				{loadedCount} releases loaded
			{/if}
		</p>
		<Button
			variant="accent"
			size="sm"
			class="min-w-40"
			aria-label="Load more releases"
			aria-describedby="artist-release-progress"
			{loading}
			onclick={onloadmore}
		>
			{loading ? 'Loading releases…' : 'Load more releases'}
		</Button>
	</div>
</Card>
