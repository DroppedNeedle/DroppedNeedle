<script lang="ts">
	import { page } from '$app/state';
	import { AUTH_FREE_PATHS } from '$lib/constants';
	import { loadAuthenticatedAppShell } from '$lib/components/lazyComponentLoaders';
	import QueryProvider from '$lib/queries/QueryProvider.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { withoutBasePath } from '$lib/utils/basePath';
	import type { Component, Snippet } from 'svelte';

	let { children }: { children: Snippet } = $props();

	type ShellComponent = Component<{ children: Snippet }>;
	let AppShell = $state<ShellComponent | null>(null);
	let shellLoadFailed = $state(false);
	let shellLoadAttempt = $state(0);
	const needsAppShell = $derived(
		!AUTH_FREE_PATHS.some((path) => withoutBasePath(page.url.pathname).startsWith(path))
	);

	$effect(() => {
		if (!needsAppShell || AppShell) return;
		const requestedAttempt = shellLoadAttempt;
		let cancelled = false;
		shellLoadFailed = false;
		void loadAuthenticatedAppShell()
			.then((component) => {
				if (!cancelled && requestedAttempt === shellLoadAttempt) AppShell = component;
			})
			.catch(() => {
				if (!cancelled && requestedAttempt === shellLoadAttempt) shellLoadFailed = true;
			});
		return () => {
			cancelled = true;
		};
	});

	function retryShellLoad(): void {
		shellLoadAttempt += 1;
	}
</script>

<QueryProvider>
	<div data-testid="app-shell" class="droppedneedle-app-shell">
		{#if needsAppShell}
			{#if AppShell}
				<AppShell {children} />
			{:else if shellLoadFailed}
				<main class="grid min-h-screen place-items-center bg-base-100 p-6">
					<div class="alert alert-error max-w-lg shadow-lg" role="alert">
						<div>
							<p class="font-semibold">DroppedNeedle did not finish loading.</p>
							<p class="mt-1 text-sm opacity-80">Check your connection, then try again.</p>
						</div>
						<Button size="sm" onclick={retryShellLoad}>Try again</Button>
					</div>
				</main>
			{:else}
				<div class="min-h-screen bg-base-100" aria-busy="true">
					<span class="sr-only">Loading DroppedNeedle</span>
				</div>
			{/if}
		{:else}
			{@render children()}
		{/if}
	</div>
</QueryProvider>

<style>
	.droppedneedle-app-shell {
		position: relative;
		isolation: isolate;
		max-width: 100vw;
		min-height: 100dvh;
		overflow-x: clip;
		background:
			radial-gradient(circle at 50% -12rem, rgb(var(--brand-library) / 0.12), transparent 22rem),
			var(--color-base-100);
	}

	/* ultra-faint global grain so every surface shares the same film stock */
	.droppedneedle-app-shell::after {
		content: '';
		position: absolute;
		inset: 0;
		z-index: -1;
		pointer-events: none;
		background-image: var(--grain-texture);
		background-size: 180px 180px;
		opacity: 0.05;
		mix-blend-mode: overlay;
	}
</style>
