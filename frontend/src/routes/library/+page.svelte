<script lang="ts">
	import { page } from '$app/state';
	import PageHeader from '$lib/components/PageHeader.svelte';
	import LibraryDashboard from '$lib/components/library/LibraryDashboard.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { Headphones, LockKeyhole, SlidersHorizontal, Waypoints, X } from 'lucide-svelte';

	const CONNECT_APPS_HREF = withBasePath('/profile#connect-apps');
	const BANNER_KEY = 'droppedneedle_connect_apps_banner_dismissed';

	let bannerDismissed = $state(true); // assume dismissed until we read storage (no SSR flash)
	const musicBrainzCallbackFailed = $derived(
		page.url.searchParams.get('musicbrainz') === 'callback-error'
	);

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
</script>

<svelte:head><title>Library · DroppedNeedle</title></svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHeader subtitle="Your scanned music library">
		{#snippet title()}Library{/snippet}
		{#snippet actions()}
			<Button
				variant="primary"
				size="sm"
				pill
				href={withBasePath('/library/local')}
				icon={Headphones}
			>
				Listen
			</Button>
			<Button size="sm" pill href={CONNECT_APPS_HREF} icon={Waypoints}>Connect Apps</Button>
			{#if authStore.isAdmin}
				<Button size="sm" pill href={withBasePath('/library/management')} icon={SlidersHorizontal}>
					Controls
				</Button>
			{:else}
				<Button
					size="sm"
					pill
					disabled
					aria-describedby="library-controls-admin-only"
					icon={LockKeyhole}
				>
					Controls
				</Button>
				<span id="library-controls-admin-only" class="sr-only">
					Library controls require administrator access.
				</span>
			{/if}
		{/snippet}
	</PageHeader>
	<div class="space-y-10 px-4 pb-12 sm:space-y-12 sm:px-6 lg:px-8">
		{#if musicBrainzCallbackFailed}
			<div class="alert alert-warning" role="alert">
				<p>
					MusicBrainz couldn't return you to the contribution. Reopen it and paste the submitted
					release URL to verify it.
				</p>
			</div>
		{/if}
		{#if !bannerDismissed}
			<Card variant="bordered" padding="md" role="note">
				<div class="flex items-center gap-3">
					<Waypoints class="hidden h-6 w-6 shrink-0 text-accent sm:block" aria-hidden="true" />
					<div class="min-w-0 flex-1">
						<p class="font-semibold">Stream this library in your favourite app</p>
						<p class="text-sm text-base-content/60">
							Connect Symfonium, Finamp and more over the OpenSubsonic or Jellyfin protocols.
						</p>
					</div>
					<Button variant="accent" size="sm" href={CONNECT_APPS_HREF}>Set up</Button>
					<IconButton size="sm" shape="square" icon={X} label="Dismiss" onclick={dismissBanner} />
				</div>
			</Card>
		{/if}
		<LibraryDashboard />
	</div>
</div>
