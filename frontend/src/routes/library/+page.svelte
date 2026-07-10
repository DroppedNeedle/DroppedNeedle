<script lang="ts">
	import PageHero from '$lib/ui/PageHero.svelte';
	import LibraryDashboard from '$lib/components/library/LibraryDashboard.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { Library, Headphones, SlidersHorizontal, Waypoints, X } from 'lucide-svelte';

	// admins land on the server-setup toggles; everyone else on their self-service Profile
	const CONNECT_APPS_HREF = $derived(
		authStore.isAdmin ? '/settings?tab=connect-apps' : '/profile#connect-apps'
	);
	const BANNER_KEY = 'droppedneedle_connect_apps_banner_dismissed';

	let bannerDismissed = $state(true); // assume dismissed until we read storage (no SSR flash)

	$effect(() => {
		if (typeof localStorage !== 'undefined') {
			bannerDismissed = localStorage.getItem(BANNER_KEY) === '1';
		}
	});

	function dismissBanner() {
		bannerDismissed = true;
		if (typeof localStorage !== 'undefined') {
			localStorage.setItem(BANNER_KEY, '1');
		}
	}

	function scrollToControls() {
		document
			.getElementById('library-controls')
			?.scrollIntoView({ behavior: 'smooth', block: 'start' });
	}
</script>

<svelte:head><title>Library · DroppedNeedle</title></svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHero
		title="Vinyl Collection"
		subtitle="Everything you own, curated in your vault."
		eyebrow="Audiophile grade"
		tint="rgb(var(--brand-library))"
	>
		{#snippet icon()}
			<Library class="h-7 w-7" />
		{/snippet}
		{#snippet actions()}
			<a
				href="/library/local"
				class="group btn btn-sm gap-2 rounded-full border-0 bg-primary text-primary-content shadow-lg shadow-primary/25 transition-all duration-200 hover:-translate-y-0.5 hover:shadow-primary/40 sm:btn-md"
			>
				<Headphones class="h-4 w-4 transition-transform duration-200 group-hover:scale-110" />
				<span>Listen</span>
			</a>
			<a
				href={CONNECT_APPS_HREF}
				class="group btn btn-sm gap-2 rounded-full border border-base-content/15 bg-base-100/50 text-base-content backdrop-blur transition-all duration-200 hover:-translate-y-0.5 hover:border-accent/40 hover:bg-base-100/80 sm:btn-md"
			>
				<Waypoints class="h-4 w-4 transition-transform duration-200 group-hover:scale-110" />
				<span>Connect Apps</span>
			</a>
			{#if authStore.isAdmin}
				<button
					onclick={scrollToControls}
					class="group btn btn-sm gap-2 rounded-full border border-base-content/15 bg-base-100/50 text-base-content backdrop-blur transition-all duration-200 hover:-translate-y-0.5 hover:border-primary/40 hover:bg-base-100/80 sm:btn-md"
				>
					<SlidersHorizontal
						class="h-4 w-4 transition-transform duration-200 group-hover:rotate-12"
					/>
					<span>Controls</span>
				</button>
			{/if}
		{/snippet}
	</PageHero>
	<div class="space-y-10 px-4 pb-12 sm:space-y-12 sm:px-6 lg:px-8">
		{#if !bannerDismissed}
			<div
				class="flex items-center gap-3 rounded-box border border-accent/25 bg-base-200 p-4"
				role="note"
			>
				<Waypoints class="hidden h-6 w-6 shrink-0 text-accent sm:block" aria-hidden="true" />
				<div class="min-w-0 flex-1">
					<p class="font-semibold">Stream this library in your favourite app</p>
					<p class="text-sm text-base-content/60">
						Connect Symfonium, Finamp and more over the OpenSubsonic or Jellyfin protocols.
					</p>
				</div>
				<a href={CONNECT_APPS_HREF} class="btn btn-sm btn-accent">Set up</a>
				<button
					class="btn btn-ghost btn-sm btn-square"
					aria-label="Dismiss"
					onclick={dismissBanner}
				>
					<X class="h-4 w-4" aria-hidden="true" />
				</button>
			</div>
		{/if}
		<LibraryDashboard />
	</div>
</div>
