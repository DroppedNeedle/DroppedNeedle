<script lang="ts">
	import { onMount } from 'svelte';

	import { Download } from 'lucide-svelte';

	import DownloadQueue from '$lib/components/downloads/DownloadQueue.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { integrationStore } from '$lib/stores/integration';

	const isAdmin = $derived(authStore.isAdmin);
	const loaded = $derived($integrationStore.loaded);
	const configured = $derived($integrationStore.download_client);

	onMount(() => {
		void integrationStore.ensureLoaded();
	});
</script>

<svelte:head>
	<title>Downloads - DroppedNeedle</title>
</svelte:head>

<div class="mx-auto w-full max-w-5xl px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
	<div class="mb-6 flex items-center gap-2">
		<Download class="h-6 w-6 text-primary" aria-hidden="true" />
		<h1 class="text-2xl font-bold sm:text-3xl">Downloads</h1>
	</div>

	{#if !loaded}
		<div class="space-y-3">
			<div class="skeleton h-10 w-64 rounded-xl"></div>
			<div class="skeleton h-20 w-full rounded-2xl"></div>
			<div class="skeleton h-20 w-full rounded-2xl"></div>
		</div>
	{:else if !configured}
		{#if isAdmin}
			<EmptyState
				icon={Download}
				title="Download client not configured"
				description="Connect a download client to request albums."
				ctaLabel="Configure Download Client"
				ctaHref="/settings?tab=download-client"
			/>
		{:else}
			<EmptyState
				icon={Download}
				title="Download client not configured"
				description="Contact your admin to configure the download client."
			/>
		{/if}
	{:else}
		<DownloadQueue />
	{/if}
</div>
