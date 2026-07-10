<script lang="ts">
	/*
	 * AppTopbar — wordmark, primary section tabs, and the command-palette
	 * trigger. Search lives in the palette (same suggest + full-search
	 * behaviour); the pill here is its always-visible entry point.
	 */
	import { page } from '$app/state';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { playerUi } from '$lib/stores/playerUi.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import ServiceHealthIndicator from '$lib/components/ServiceHealthIndicator.svelte';
	import { UserRound, PanelRight, Search, Settings, Menu } from 'lucide-svelte';

	const currentPath = $derived(page.url.pathname);

	const tabs = [
		{ label: 'Explore', href: '/', exact: true },
		{ label: 'Library', href: '/library', exact: false },
		{ label: 'Stations', href: '/stations', exact: false }
	];

	function isActive(tab: (typeof tabs)[number]): boolean {
		return tab.exact ? currentPath === tab.href : currentPath.startsWith(tab.href);
	}

	function openPalette(): void {
		(document.getElementById('search_modal') as HTMLDialogElement)?.showModal();
	}

	const isMac =
		typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '');
</script>

<header class="droppedneedle-topbar dn-topbar sticky top-0 z-50">
	<label
		for="main-drawer"
		class="dn-topbar__profile cursor-pointer md:hidden"
		aria-label="Open navigation drawer"
	>
		<Menu class="h-5 w-5" />
	</label>

	<a href="/" class="dn-topbar__brand" aria-label="Home">
		<span class="dn-topbar__wordmark">DroppedNeedle</span>
	</a>

	<nav class="dn-topbar__tabs" aria-label="Sections">
		{#each tabs as tab (tab.href)}
			<a
				href={tab.href}
				class="dn-topbar__tab"
				class:dn-topbar__tab--active={isActive(tab)}
				aria-current={isActive(tab) ? 'page' : undefined}
			>
				{tab.label}
			</a>
		{/each}
	</nav>

	<div class="dn-topbar__actions">
		<button class="dn-topbar__palette" onclick={openPalette} aria-label="Open command palette">
			<Search class="h-3.5 w-3.5 shrink-0 opacity-60" />
			<span class="dn-topbar__palette-label">Command Palette</span>
			<kbd class="dn-topbar__palette-kbd">{isMac ? '⌘' : 'Ctrl'}+K</kbd>
		</button>

		<ServiceHealthIndicator />

		{#if playerStore.isPlayerVisible}
			<button
				class="dn-topbar__profile hidden 2xl:inline-flex"
				onclick={() => playerUi.toggleSidePanel()}
				aria-label={playerUi.sidePanelOpen ? 'Hide now playing panel' : 'Show now playing panel'}
				aria-pressed={playerUi.sidePanelOpen}
			>
				<PanelRight class="h-5 w-5" />
			</button>
		{/if}

		{#if authStore.isAdmin}
			<a href="/settings" class="dn-topbar__profile hidden md:inline-flex" aria-label="Settings">
				<Settings class="h-5 w-5" />
			</a>
		{/if}

		<a href="/profile" class="dn-topbar__profile" aria-label="Profile">
			{#if authStore.user?.avatar_url}
				<img
					src={authStore.user.avatar_url}
					alt="Profile"
					class="h-8 w-8 rounded-full object-cover"
				/>
			{:else}
				<UserRound class="h-5 w-5" />
			{/if}
		</a>
	</div>
</header>
