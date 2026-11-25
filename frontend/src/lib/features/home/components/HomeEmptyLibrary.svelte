<!--
	Home's welcome while the library holds nothing. An admin with a library
	folder starts a scan from here; without one, the button leads to
	Settings > Library to add it.
-->
<script lang="ts">
	import { Music } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { getTargetLibrarySettingsQuery } from '$lib/features/library-management/queries/LibraryPolicyQueries.svelte';
	import { createRequestLibraryRunMutation } from '$lib/features/library-management/queries/LibraryOperationMutations.svelte';
	import Button from '$lib/ui/Button.svelte';

	const settingsQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const requestRun = createRequestLibraryRunMutation();
	const settings = $derived(settingsQuery.data);
	const libraryHref = withBasePath('/settings?tab=library');

	function startScan(): void {
		if (!settings) return;
		requestRun.mutate({
			kind: 'incremental',
			scope_ids: [],
			expected_policy_revision: settings.policy_revision
		});
	}
</script>

<div class="flex flex-col items-center justify-center py-12 sm:py-16">
	<Music class="h-12 w-12 sm:h-16 sm:w-16 mb-4 sm:mb-6" />
	<h2 class="mb-2 text-center text-3xl font-bold sm:text-4xl lg:text-5xl">
		Welcome to <span class="text-primary">DroppedNeedle</span>
	</h2>
	{#if authStore.isAdmin}
		<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
			Your library is empty. Add a library path and start a scan to fill it.
		</p>
		<div class="flex flex-wrap justify-center gap-2">
			{#if !settings}
				<Button variant="primary" href={libraryHref}>Library settings</Button>
			{:else if settings.library_roots.length === 0}
				<Button variant="primary" href={libraryHref}>Add a library folder</Button>
			{:else if !settings.enabled}
				<Button variant="primary" href={libraryHref}>Turn on the library</Button>
			{:else}
				<Button
					variant="primary"
					loading={requestRun.isPending}
					disabled={requestRun.isPending}
					onclick={startScan}
				>
					Start scan
				</Button>
			{/if}
			<Button variant="ghost" href={withBasePath('/library')}>Go to Library</Button>
		</div>
	{:else}
		<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
			Your library is being prepared. An admin is setting things up - check back soon.
		</p>
		<Button variant="ghost" href={withBasePath('/library')}>Go to Library</Button>
	{/if}
</div>
