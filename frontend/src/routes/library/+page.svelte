<script lang="ts">
	import { page } from '$app/state';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import LibraryDashboard from '$lib/features/library/components/LibraryDashboard.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
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
	<ListPageHeader title="Library" subtitle="Your scanned music library" ticket="DN / CC 3.2">
		{#snippet actions()}
			<Button
				variant="primary"
				size="display"
				href={withBasePath('/library/local')}
				icon={Headphones}
			>
				Listen
			</Button>
			<Button variant="outline" size="display" href={CONNECT_APPS_HREF} icon={Waypoints}>
				Connect Apps
			</Button>
			{#if authStore.isAdmin}
				<Button
					variant="outline"
					size="display"
					href={withBasePath('/library/management')}
					icon={SlidersHorizontal}
				>
					Controls
				</Button>
			{:else}
				<Button
					variant="outline"
					size="display"
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
	</ListPageHeader>
	<div class="space-y-5 px-4 pb-8 sm:px-6 lg:px-8">
		{#if musicBrainzCallbackFailed}
			<div class="alert alert-warning" role="alert">
				<p>
					MusicBrainz couldn't return you to the contribution. Reopen it and paste the submitted
					release URL to verify it.
				</p>
			</div>
		{/if}
		{#if !bannerDismissed}
			<div class="connect-banner" role="note">
				<Waypoints class="connect-banner__icon" aria-hidden="true" />
				<div class="min-w-0 flex-1">
					<p class="connect-banner__title">Stream this library in your favourite app</p>
					<p class="connect-banner__body">
						Connect Symfonium, Finamp and more over the OpenSubsonic or Jellyfin protocols.
					</p>
				</div>
				<Button variant="primary" size="display-sm" href={CONNECT_APPS_HREF}>Set up</Button>
				<IconButton size="sm" shape="square" icon={X} label="Dismiss" onclick={dismissBanner} />
			</div>
		{/if}
		<LibraryDashboard />
	</div>
</div>

<style>
	.connect-banner {
		display: flex;
		align-items: center;
		gap: 1rem;
		padding: 0.75rem 1rem 0.75rem 1.5rem;
		border: 1px solid color-mix(in oklab, var(--dn-title) 45%, transparent);
		border-radius: 0.75rem;
		background-color: color-mix(in oklab, var(--color-base-200) 70%, transparent);
	}
	.connect-banner :global(.connect-banner__icon) {
		flex: none;
		width: 2rem;
		height: 2rem;
		color: var(--dn-title);
	}
	.connect-banner__title {
		font-family: var(--font-display);
		font-size: 1.375rem;
		line-height: 1.1;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.connect-banner__body {
		margin-top: 0.125rem;
		font-size: 0.9375rem;
		color: var(--color-base-content);
	}
	@media (max-width: 767px) {
		.connect-banner {
			gap: 0.625rem;
			padding: 0.875rem 0.5rem 0.875rem 0.875rem;
		}
		.connect-banner :global(.connect-banner__icon) {
			width: 1.75rem;
			height: 1.75rem;
		}
		.connect-banner__title {
			font-size: 1.0625rem;
		}
		.connect-banner__body {
			font-size: 0.6875rem;
			line-height: 1.35;
		}
		.connect-banner :global(.btn) {
			--size: 2rem;
			--btn-p: 0.625rem;
			--fontsize: 0.875rem;
		}
	}
</style>
