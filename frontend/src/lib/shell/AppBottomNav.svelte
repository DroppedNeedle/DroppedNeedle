<script lang="ts">
	/*
	 * AppBottomNav — mobile bottom navigation (shown < md).
	 * Four user-configurable slots (Settings → Appearance) + a fixed Menu slot
	 * that opens the app drawer. Live badges follow their destinations.
	 */
	import { page } from '$app/state';
	import { syncStatus } from '$lib/stores/syncStatus.svelte';
	import { downloadsActivity } from '$lib/stores/downloadsActivity.svelte';
	import { appearance } from '$lib/stores/appearance.svelte';
	import { bottomNavItem } from '$lib/shell/bottomNavItems';
	import { Menu, ArrowUpCircle } from 'lucide-svelte';

	let {
		versionUpdateAvailable = false,
		libraryScanActive = false
	}: { versionUpdateAvailable?: boolean; libraryScanActive?: boolean } = $props();

	const currentPath = $derived(page.url.pathname);
	const slots = $derived(appearance.bottomNav.map(bottomNavItem));

	function isActive(item: { id: string; match?: string }): boolean {
		const match = item.match;
		if (!match) return false;
		if (match === '/') return currentPath === '/';
		const hit = currentPath === match || currentPath.startsWith(`${match}/`);
		// Collection's /library prefix must not claim Recent Spun's route
		if (item.id === 'collection' && currentPath.startsWith('/library/tracks')) return false;
		return hit;
	}

	function openSearch(): void {
		(document.getElementById('search_modal') as HTMLDialogElement)?.showModal();
	}
</script>

<nav class="droppedneedle-bottom-nav md:hidden" aria-label="Primary navigation">
	{#each slots as item (item.id)}
		{#if item.action === 'search'}
			<button
				type="button"
				class="droppedneedle-bottom-nav__item"
				class:active={isActive(item)}
				onclick={openSearch}
				aria-current={isActive(item) ? 'page' : undefined}
			>
				<item.icon />
				<span>{item.label}</span>
			</button>
		{:else}
			<a
				href={item.href}
				class="droppedneedle-bottom-nav__item"
				class:active={isActive(item)}
				aria-current={isActive(item) ? 'page' : undefined}
			>
				<item.icon />
				<span>{item.label}</span>
				{#if item.id === 'collection' && (syncStatus.isActive || libraryScanActive)}
					<span class="droppedneedle-bottom-nav__badge" aria-label="Library sync in progress"
					></span>
				{:else if item.id === 'downloads' && downloadsActivity.isActive}
					<span class="droppedneedle-bottom-nav__badge" aria-label="Active downloads"
						>{downloadsActivity.count}</span
					>
				{/if}
			</a>
		{/if}
	{/each}

	<label
		for="main-drawer"
		class="droppedneedle-bottom-nav__item cursor-pointer"
		aria-label="Open navigation drawer"
	>
		<Menu />
		<span>Menu</span>
		{#if versionUpdateAvailable}
			<span class="droppedneedle-bottom-nav__badge" aria-label="Update available">
				<ArrowUpCircle class="h-3 w-3" />
			</span>
		{/if}
	</label>
</nav>
