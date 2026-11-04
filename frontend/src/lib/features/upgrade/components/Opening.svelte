<script lang="ts">
	import { getUpgradeAppReadyQuery } from '$lib/features/upgrade/queries/UpgradeQueries.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	// The app takes the upgrade page's place on the same address. Once it
	// answers, a full load lets it start clean and show its own sign-in, or
	// its first-admin setup after a fresh start.
	const ready = getUpgradeAppReadyQuery(() => true);

	$effect(() => {
		if (ready.isSuccess) window.location.assign(withBasePath('/'));
	});

	const slow = $derived(ready.failureCount >= 30);
</script>

<Card variant="bordered" title="Opening DroppedNeedle">
	<div class="flex items-center gap-3 text-sm" aria-busy="true">
		<span class="loading loading-spinner loading-sm motion-keep"></span>
		<span>Starting the app.</span>
	</div>
	{#if slow}
		<p class="mt-3 text-sm text-muted" role="status">
			This is taking a while. The server log says what it is doing.
		</p>
	{/if}
</Card>
