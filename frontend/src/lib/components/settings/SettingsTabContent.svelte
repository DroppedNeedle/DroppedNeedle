<script lang="ts">
	import type { Component } from 'svelte';

	import Button from '$lib/ui/Button.svelte';
	import { loadSettingsTab } from './settingsTabs';

	interface Props {
		tab: string;
		isAdmin: boolean;
	}

	let { tab, isAdmin }: Props = $props();
	let ActiveTab = $state<Component | null>(null);
	let loadedTab = $state<string | null>(null);
	let loadFailed = $state(false);
	let loadAttempt = $state(0);

	$effect(() => {
		const requestedTab = tab;
		const requestedAdminState = isAdmin;
		const requestedAttempt = loadAttempt;
		let cancelled = false;
		ActiveTab = null;
		loadedTab = null;
		loadFailed = false;
		void loadSettingsTab(requestedTab, requestedAdminState)
			.then((component) => {
				if (cancelled || requestedAttempt !== loadAttempt) return;
				if (component === null) {
					loadFailed = true;
					return;
				}
				ActiveTab = component;
				loadedTab = requestedTab;
			})
			.catch(() => {
				if (!cancelled && requestedAttempt === loadAttempt) loadFailed = true;
			});
		return () => {
			cancelled = true;
		};
	});

	function retryLoad(): void {
		loadAttempt += 1;
	}
</script>

{#if ActiveTab && loadedTab === tab}
	<ActiveTab />
{:else if loadFailed}
	<div class="alert alert-error" role="alert">
		<div>
			<p class="font-semibold">This settings section did not load.</p>
			<p class="mt-1 text-sm opacity-80">Check your connection, then try again.</p>
		</div>
		<Button size="sm" onclick={retryLoad}>Try again</Button>
	</div>
{:else}
	<div class="space-y-4" aria-busy="true" aria-label="Loading settings">
		<div class="skeleton h-8 w-52"></div>
		<div class="skeleton h-24 w-full"></div>
		<div class="skeleton h-40 w-full"></div>
	</div>
{/if}
